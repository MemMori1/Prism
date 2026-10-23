//! 脚本运行时：在**固定环境**里跑一段模型写出来的代码。
//!
//! 和 `run_command` 的区别不是「跑什么」，而是「在哪跑」：
//!
//! | | `run_command` | `run_script`（本模块） |
//! |---|---|---|
//! | 入口 | 模型的 shell 命令 | 模型写的脚本正文 |
//! | 环境变量 | 白名单（密钥一律不带） | 同左 |
//! | 工作目录 | 会话目录 | 会话目录（写入范围也只到这里） |
//! | 隔离 | 无（只有策略闸门） | macOS：seatbelt；Windows：策略层 |
//! | 网络 | 跟随命令 | 默认关（要开得单独批准） |
//!
//! macOS 用系统自带的 `sandbox-exec` + 一份内联 profile：默认允许，但**写只允许
//! 工作目录 / 沙箱目录 / 临时目录**，个人目录（Desktop、Downloads、Documents…）
//! 与钥匙串一律不可写，网络默认拒。Windows 没有等价的内置沙箱，这里只做环境
//! 清洗 + 工作目录约束，并在结果里**明说没有 OS 级隔离**——不假装有。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::guard;

/// 一次脚本执行的上限：再长就该转后台任务，而不是占着一轮。
const DEFAULT_TIMEOUT_MS: u64 = 60_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
/// 输出上限（字符）：脚本打印一屏就够了，多出来的会挤掉上下文。
const MAX_OUTPUT_CHARS: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Python,
    Node,
    Shell,
    PowerShell,
}

impl Lang {
    /// 支持的语言名（含常见别名）。报错时把这份清单原样给模型，省一轮试错。
    pub fn parse(raw: &str) -> Result<Lang, String> {
        let key = raw.trim().to_lowercase();
        match key.as_str() {
            "python" | "python3" | "py" => Ok(Lang::Python),
            "node" | "js" | "javascript" | "nodejs" | "ts" | "typescript" => Ok(Lang::Node),
            "sh" | "bash" | "shell" | "zsh" => Ok(Lang::Shell),
            "powershell" | "pwsh" | "ps1" => Ok(Lang::PowerShell),
            other => Err(format!(
                "不认识的语言「{other}」：支持 python / node / sh / powershell"
            )),
        }
    }

    fn ext(self) -> &'static str {
        match self {
            Lang::Python => "py",
            Lang::Node => "mjs",
            Lang::Shell => "sh",
            Lang::PowerShell => "ps1",
        }
    }

    /// 候选解释器，按顺序探测（第一个找到的用）。
    fn interpreters(self) -> &'static [&'static str] {
        match self {
            Lang::Python => &["python3", "python"],
            Lang::Node => &["node"],
            Lang::Shell => &["bash", "sh"],
            Lang::PowerShell => &["pwsh", "powershell"],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::Node => "node",
            Lang::Shell => "shell",
            Lang::PowerShell => "powershell",
        }
    }
}

/// 执行选项。`network` 与 `disable_sandbox` 都要上游审批过才敢传 true。
#[derive(Debug, Clone)]
pub struct Options {
    pub timeout_ms: u64,
    pub network: bool,
    pub disable_sandbox: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            timeout_ms: DEFAULT_TIMEOUT_MS,
            network: false,
            disable_sandbox: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    /// 给模型看的全文（头部是事实：解释器、沙箱、退出码）。
    pub text: String,
    pub exit: i32,
    pub timed_out: bool,
    /// 是否真的进了 OS 级沙箱。
    pub isolated: bool,
    pub ok: bool,
}

/// 在 PATH 里找可执行文件（Windows 额外补 `.exe` / `.cmd` / `.bat`）。
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string())
            .split(';')
            .map(|item| item.to_lowercase())
            .collect()
    } else {
        Vec::new()
    };
    for dir in std::env::split_paths(&path) {
        let direct = dir.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn resolve_interpreter(lang: Lang) -> Result<PathBuf, String> {
    for name in lang.interpreters() {
        if let Some(path) = which(name) {
            return Ok(path);
        }
    }
    Err(format!(
        "这台机器上没有 {} 的解释器（找过：{}）。装一个，或换 run_command 用现成的工具链",
        lang.label(),
        lang.interpreters().join(" / ")
    ))
}

/// seatbelt profile 里的字符串转义：路径里可能有引号或反斜杠。
fn seatbelt_quote(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{text}\"")
}

/// 生成 macOS 的 seatbelt profile（纯函数，方便单测）。
///
/// 顺序有意义：seatbelt 是「最后一条匹配的规则生效」，所以先 `deny file-write*`
/// 再逐个 `allow` 要放开的位置。
pub fn seatbelt_profile(workspace: &Path, sandbox: &Path, network: bool) -> String {
    let mut writes = vec![
        workspace.to_path_buf(),
        sandbox.to_path_buf(),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/tmp"),
        PathBuf::from("/private/var/tmp"),
    ];
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        writes.push(PathBuf::from(tmp));
    }
    let mut profile = String::from("(version 1)\n(allow default)\n");
    if !network {
        profile.push_str("(deny network*)\n");
    }
    profile.push_str("(deny file-write*)\n");
    profile.push_str("(allow file-write*\n");
    for path in &writes {
        profile.push_str(&format!("  (subpath {})\n", seatbelt_quote(path)));
    }
    for device in ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty", "/dev/dtracehelper"] {
        profile.push_str(&format!("  (literal {})\n", seatbelt_quote(Path::new(device))));
    }
    profile.push_str(")\n");
    // 个人目录与钥匙串：连读都挡掉一层（脚本没必要翻你桌面）。
    if let Some(home) = guard::home() {
        let mut deny = Vec::new();
        for name in ["Desktop", "Downloads", "Documents", "Library", "Pictures", ".ssh", ".aws"] {
            let path = home.join(name);
            if path.exists() {
                deny.push(path);
            }
        }
        if !deny.is_empty() {
            profile.push_str("(deny file-read*\n");
            for path in deny {
                profile.push_str(&format!("  (subpath {})\n", seatbelt_quote(&path)));
            }
            profile.push_str(")\n");
        }
    }
    profile
}

fn sandbox_dir() -> Result<PathBuf, String> {
    let base = crate::engines::opencode::search::prism_dir()
        .ok_or_else(|| "无法定位 ~/.prism 目录".to_string())?;
    let dir = base.join("sandbox");
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建沙箱目录失败：{error}"))?;
    Ok(dir)
}

/// 每次执行一个独立目录。
///
/// 毫秒不够：同一毫秒里并行跑两次（测试就是这么跑的）会共用一个 `script.sh`，
/// 一个把另一个的脚本覆盖掉。加进程内自增序号才是唯一的。
fn stamp() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{millis}-{seq}")
}

/// 只留最近 20 次执行的目录。
///
/// 每次跑都留一份脚本与输出，攒久了是一堆无意义的小文件；保留最近几次够排查了。
fn prune_old_runs(current: &Path) {
    const KEEP: usize = 20;
    let Some(parent) = current.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path != current)
        .collect();
    if dirs.len() <= KEEP {
        return;
    }
    dirs.sort();
    let remove = dirs.len() - KEEP;
    for path in dirs.into_iter().take(remove) {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn clamp(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n…（输出过长，已截断）")
}

/// 拼一条执行命令（沙箱与否只是前面多套一层 `sandbox-exec`）。
fn build_command(
    interpreter: &Path,
    script: &Path,
    workspace: &Path,
    sandbox_dir: &Path,
    seatbelt: Option<&Path>,
    network: bool,
) -> Command {
    let mut command = match seatbelt {
        Some(exec) => {
            let profile = seatbelt_profile(workspace, sandbox_dir, network);
            let mut cmd = Command::new(exec);
            cmd.arg("-p").arg(profile).arg(interpreter).arg(script);
            cmd
        }
        None => {
            let mut cmd = Command::new(interpreter);
            cmd.arg(script);
            cmd
        }
    };
    command
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 环境清洗：只给白名单，密钥类一律不带（`env` 一打印就全在上下文里了）。
    command.env_clear();
    for (name, value) in guard::sanitized_env() {
        command.env(name, value);
    }
    // 脚本里 `python3 x.py` 之类的自调用要能找到同一个解释器。
    if let Some(dir) = interpreter.parent() {
        let path = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{}:{}", dir.display(), path));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 独立进程组：停止时要连它拉起的子进程一起收。
        command.process_group(0);
    }
    command
}

struct RunResult {
    ending: &'static str,
    exit: i32,
    stdout: String,
    stderr: String,
}

fn wait_command(
    mut command: Command,
    timeout: Duration,
    should_stop: &dyn Fn() -> bool,
) -> Result<RunResult, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动失败：{error}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = stdout.map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let err_reader = stderr.map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    let started = Instant::now();
    let ending = loop {
        match child.try_wait() {
            Ok(Some(_)) => break "done",
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                return Err(format!("等待脚本失败：{error}"));
            }
        }
        if should_stop() {
            let _ = child.kill();
            break "stopped";
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            break "timeout";
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let status = child.wait().ok();
    let out = out_reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let err = err_reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    Ok(RunResult {
        ending,
        exit: status.and_then(|value| value.code()).unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out).to_string(),
        stderr: String::from_utf8_lossy(&err).to_string(),
    })
}

/// `sandbox_apply` 失败 = 这台机器不允许再套一层沙箱（已有外层沙箱 / MDM 策略）。
fn sandbox_refused(result: &RunResult) -> bool {
    result.exit == 71 && result.stderr.contains("sandbox_apply")
}

/// 跑一段脚本。`workspace` 是会话目录（能写的地方），`should_stop` 每 50ms 问一次。
///
/// 沙箱申请失败会**退回策略层重跑一次**，并在结果头里明写「沙箱没生效」——
/// 悄悄降级比没有沙箱更糟：用户以为自己被隔离了。
pub fn run(
    lang: Lang,
    code: &str,
    workspace: &Path,
    options: &Options,
    should_stop: &dyn Fn() -> bool,
) -> Result<Outcome, String> {
    if code.trim().is_empty() {
        return Err("脚本正文是空的".to_string());
    }
    let interpreter = resolve_interpreter(lang)?;
    let dir = sandbox_dir()?.join(stamp());
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建本次沙箱失败：{error}"))?;
    prune_old_runs(&dir);
    let script = dir.join(format!("script.{}", lang.ext()));
    std::fs::write(&script, code).map_err(|error| format!("写脚本失败：{error}"))?;

    let seatbelt = if cfg!(target_os = "macos") && !options.disable_sandbox {
        which("sandbox-exec")
    } else {
        None
    };
    let timeout = Duration::from_millis(options.timeout_ms.clamp(500, MAX_TIMEOUT_MS));

    let mut isolated = seatbelt.is_some();
    let mut note = String::new();
    let mut result = wait_command(
        build_command(
            &interpreter,
            &script,
            workspace,
            &dir,
            seatbelt.as_deref(),
            options.network,
        ),
        timeout,
        should_stop,
    )
    .map_err(|error| format!("{error}（解释器：{}）", interpreter.display()))?;

    if isolated && sandbox_refused(&result) {
        // 再套一层被拒：退回策略层重跑，并如实说明。
        isolated = false;
        note = format!(
            "沙箱没能应用（{}）：已退回策略层执行——环境仍是清洗过的，但没有 OS 级隔离。\n",
            result.stderr.trim().lines().next().unwrap_or("sandbox_apply 失败")
        );
        result = wait_command(
            build_command(&interpreter, &script, workspace, &dir, None, options.network),
            timeout,
            should_stop,
        )
        .map_err(|error| format!("{error}（解释器：{}）", interpreter.display()))?;
    }

    let mut text = String::new();
    text.push_str(&note);
    text.push_str(&format!(
        "{} · 解释器 {}\n",
        if isolated {
            if options.network {
                "沙箱：seatbelt（写入仅限工作目录与临时目录；网络：开）"
            } else {
                "沙箱：seatbelt（写入仅限工作目录与临时目录；网络：关）"
            }
        } else if options.disable_sandbox {
            "沙箱：已按要求关闭（以你的身份直接跑）"
        } else {
            "沙箱：无 OS 级隔离（只有策略闸门与环境清洗）"
        },
        interpreter.display()
    ));
    text.push_str(&format!("脚本：{}\n", script.display()));
    match result.ending {
        "done" => text.push_str(&format!("退出码：{}\n", result.exit)),
        "stopped" => text.push_str(&format!("已被用户停止，脚本已终止（退出码 {}）\n", result.exit)),
        _ => text.push_str(&format!(
            "超时（{} ms）已终止（退出码 {}）\n",
            timeout.as_millis(),
            result.exit
        )),
    }
    if !result.stdout.trim().is_empty() {
        text.push_str("--- stdout ---\n");
        text.push_str(result.stdout.trim_end());
        text.push('\n');
    }
    if !result.stderr.trim().is_empty() {
        text.push_str("--- stderr ---\n");
        text.push_str(result.stderr.trim_end());
        text.push('\n');
    }
    if result.stdout.trim().is_empty() && result.stderr.trim().is_empty() {
        text.push_str("（脚本没有任何输出）\n");
    }
    Ok(Outcome {
        text: clamp(&text),
        exit: result.exit,
        timed_out: result.ending == "timeout",
        isolated,
        ok: result.ending == "done" && result.exit == 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_parse_with_aliases() {
        assert_eq!(Lang::parse("python3").unwrap(), Lang::Python);
        assert_eq!(Lang::parse("JS").unwrap(), Lang::Node);
        assert_eq!(Lang::parse("bash").unwrap(), Lang::Shell);
        assert_eq!(Lang::parse("pwsh").unwrap(), Lang::PowerShell);
        let error = Lang::parse("ruby").unwrap_err();
        assert!(error.contains("支持"), "{error}");
    }

    #[test]
    fn profile_denies_writes_then_allows_the_workspace() {
        let profile = seatbelt_profile(
            Path::new("/tmp/work space"),
            Path::new("/tmp/sandbox"),
            false,
        );
        assert!(profile.starts_with("(version 1)"), "{profile}");
        assert!(profile.contains("(deny network*)"), "{profile}");
        // deny 必须出现在 allow 之前：seatbelt 是最后匹配生效。
        let deny = profile.find("(deny file-write*)").expect("有 deny");
        let allow = profile.find("(allow file-write*").expect("有 allow");
        assert!(deny < allow, "{profile}");
        assert!(profile.contains("/tmp/work space"), "{profile}");
        assert!(profile.contains("/dev/null"), "{profile}");
    }

    #[test]
    fn profile_can_allow_network() {
        let profile = seatbelt_profile(Path::new("/tmp/w"), Path::new("/tmp/s"), true);
        assert!(!profile.contains("(deny network*)"), "{profile}");
    }

    /// profile 里的路径要能顶住引号（否则 profile 语法就断了）。
    #[test]
    fn profile_escapes_quotes() {
        let profile = seatbelt_profile(Path::new("/tmp/a\"b"), Path::new("/tmp/s"), false);
        assert!(profile.contains("a\\\"b"), "{profile}");
    }

    #[test]
    fn which_finds_a_shell_everywhere() {
        // 两个平台都必然有 sh 或 cmd；用 PATH 里一定存在的名字验证探测逻辑。
        let probe = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(which(probe).is_some(), "PATH 里应该能找到 {probe}");
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn empty_script_is_rejected_before_spawning() {
        let error = run(
            Lang::Shell,
            "   ",
            &std::env::temp_dir(),
            &Options::default(),
            &|| false,
        )
        .unwrap_err();
        assert!(error.contains("空的"), "{error}");
    }

    /// 真跑一段 shell（不依赖 python / node 是否存在），确认：
    /// 输出拿得到、环境被清洗、退出码传回来。
    #[test]
    fn shell_script_runs_with_scrubbed_env() {
        let dir = std::env::temp_dir().join(format!("prism-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let code = "echo hello; echo \"KEY=[${OPENAI_API_KEY:-none}]\"";
        let outcome = run(Lang::Shell, code, &dir, &Options::default(), &|| false)
            .expect("跑起来");
        assert!(outcome.text.contains("hello"), "{}", outcome.text);
        // 进程环境里就算有 OPENAI_API_KEY，也不该传进子进程。
        assert!(outcome.text.contains("KEY=[none]"), "{}", outcome.text);
        assert_eq!(outcome.exit, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 装了 python 就该跑得起来：这条用例同时验「探测到解释器」与「stdout 拿得到」。
    #[test]
    fn python_script_runs_when_available() {
        if which("python3").is_none() && which("python").is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("prism-rt-py-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let code = "import sys\nprint('py-ok', sys.version_info.major)\n";
        let outcome = run(Lang::Python, code, &dir, &Options::default(), &|| false).expect("跑起来");
        assert!(outcome.text.contains("py-ok 3"), "{}", outcome.text);
        assert!(outcome.ok, "{}", outcome.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// node 同理：`console.log` 能出来，环境里也不该有密钥。
    #[test]
    fn node_script_runs_when_available() {
        if which("node").is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("prism-rt-node-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let code = "console.log('node-ok', process.version.split('.')[0], process.env.OPENAI_API_KEY ?? 'none');";
        let outcome = run(Lang::Node, code, &dir, &Options::default(), &|| false).expect("跑起来");
        assert!(outcome.text.contains("node-ok v"), "{}", outcome.text);
        assert!(outcome.text.contains("none"), "密钥不该进子进程：{}", outcome.text);
        assert!(outcome.ok, "{}", outcome.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timeout_kills_the_script() {
        let dir = std::env::temp_dir().join(format!("prism-rt-slow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let options = Options {
            timeout_ms: 600,
            ..Options::default()
        };
        let outcome = run(Lang::Shell, "sleep 5; echo done", &dir, &options, &|| false)
            .expect("跑起来");
        assert!(outcome.timed_out, "{}", outcome.text);
        assert!(!outcome.text.contains("done"), "{}", outcome.text);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
