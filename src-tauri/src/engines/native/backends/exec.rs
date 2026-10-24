//! 外部程序的统一执行入口。
//!
//! 反汇编器 / 脱壳器 / 反编译器都是"拉一个进程、喂一个文件、收一段文本"。
//! 各写各的 `Command::new` 会重复四件事，而且四件都容易漏：**超时、输出上限、
//! 进程组回收、环境清洗**。这里只写一遍。
//!
//! 三条纪律（和 `docs/BIN_TOOLCHAIN.md` §5 对应）：
//!
//! 1. **argv 数组，不走 shell**：样本文件名里带 `;` `$()` 也只是参数；
//! 2. **输出是头 + 尾**：只留尾部 JSON 从头就崩，只留头部看不出哪一步失败；
//! 3. **杀进程杀整组**：`upx` / `r2` 会拉子进程，只杀父进程等于没杀。

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::super::guard;

/// 轮询子进程状态的间隔（也是停止与超时的检查粒度）。
const POLL: Duration = Duration::from_millis(25);
/// SIGTERM 之后等多久补 SIGKILL。
const KILL_GRACE: Duration = Duration::from_millis(900);
/// 读管道的块大小。
const CHUNK: usize = 8 * 1024;

#[derive(Debug, Clone)]
pub struct ExecSpec {
    pub program: PathBuf,
    /// argv 数组。**不要**在这里拼 `sh -c`。
    pub args: Vec<String>,
    /// 工作目录；`None` = 继承当前进程。
    pub cwd: Option<PathBuf>,
    /// 在 [`guard::sanitized_env`] 之上叠加的变量（`JAVA_HOME` 之类）。
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    /// 单路输出最多留多少字节（头 + 尾，见 [`CapBuffer`]）。
    pub stdout_cap: usize,
    pub stderr_cap: usize,
}

impl ExecSpec {
    pub fn new(program: impl Into<PathBuf>) -> ExecSpec {
        ExecSpec {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            timeout: Duration::from_secs(120),
            stdout_cap: 2 * 1024 * 1024,
            stderr_cap: 512 * 1024,
        }
    }

    pub fn arg(mut self, value: impl Into<String>) -> ExecSpec {
        self.args.push(value.into());
        self
    }

    pub fn args<I, S>(mut self, values: I) -> ExecSpec
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(values.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> ExecSpec {
        self.cwd = Some(dir.into());
        self
    }

    /// 在 [`guard::sanitized_env`] 之上叠加的变量（`JAVA_HOME` 之类）。
    ///
    /// 现在还没有工具用得上（upx 不吃环境变量）；Ghidra 接进来时靠它传
    /// `JAVA_HOME`，所以接口先留着。
    #[allow(dead_code)]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> ExecSpec {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> ExecSpec {
        self.timeout = timeout;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExecOutput {
    /// 退出码；被信号杀掉时为 `None`。
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub duration: Duration,
}

impl ExecOutput {
    /// 正常退出（没超时、退出码 0）。
    pub fn ok(&self) -> bool {
        !self.timed_out && self.code == Some(0)
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }

    /// 给错误文案用的一段人话：优先 stderr 的末几行，其次 stdout。
    pub fn tail_text(&self, limit: usize) -> String {
        let source = if self.stderr.iter().any(|byte| !byte.is_ascii_whitespace()) {
            self.stderr_text()
        } else {
            self.stdout_text()
        };
        let text = source.trim();
        if text.is_empty() {
            return "（没有任何输出）".to_string();
        }
        let tail: String = text
            .chars()
            .rev()
            .take(limit)
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        tail
    }
}

/// 头 + 尾的定长缓冲。
///
/// 之所以不是"留最后 N 字节"：JSON 从中间截断会连第一行都解析不了，
/// 而错误往往发生在开头（版本不对、参数不认）。头 64 KiB 够看开头，
/// 其余留给尾部——失败原因通常在最后几行。
#[derive(Debug)]
struct CapBuffer {
    head: Vec<u8>,
    tail: Vec<u8>,
    head_cap: usize,
    tail_cap: usize,
    dropped: usize,
}

impl CapBuffer {
    fn new(cap: usize) -> CapBuffer {
        let head_cap = cap.min(64 * 1024);
        let tail_cap = cap.saturating_sub(head_cap);
        CapBuffer {
            head: Vec::new(),
            tail: Vec::new(),
            head_cap,
            tail_cap,
            dropped: 0,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        let mut rest = chunk;
        if self.head.len() < self.head_cap {
            let take = (self.head_cap - self.head.len()).min(rest.len());
            self.head.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
        }
        if rest.is_empty() {
            return;
        }
        if self.tail_cap == 0 {
            self.dropped += rest.len();
            return;
        }
        self.tail.extend_from_slice(rest);
        if self.tail.len() > self.tail_cap {
            let cut = self.tail.len() - self.tail_cap;
            self.tail.drain(..cut);
            self.dropped += cut;
        }
    }

    fn finish(mut self) -> (Vec<u8>, bool) {
        if self.dropped == 0 {
            return (self.head, false);
        }
        let marker = format!("\n…（截断 {} 字节）…\n", self.dropped);
        self.head.extend_from_slice(marker.as_bytes());
        self.head.extend_from_slice(&self.tail);
        (self.head, true)
    }
}

fn pump<R: Read + Send + 'static>(mut reader: R, cap: usize) -> mpsc::Receiver<(Vec<u8>, bool)> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = CapBuffer::new(cap);
        let mut chunk = [0u8; CHUNK];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => buffer.push(&chunk[..read]),
                Err(_) => break,
            }
        }
        let _ = tx.send(buffer.finish());
    });
    rx
}

/// 跑一次外部程序。`Err` 只表示"根本没起来"（找不到、没权限）。
///
/// 非零退出、超时都算"跑起来了但失败"，由调用方看 [`ExecOutput`] 决定怎么说。
pub fn run(spec: &ExecSpec) -> Result<ExecOutput, String> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    if let Some(dir) = &spec.cwd {
        command.current_dir(dir);
    }
    for (key, value) in guard::sanitized_env() {
        command.env(key, value);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 自领进程组：停止时能连同孙子进程一起收掉。
        command.process_group(0);
    }

    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|error| format!("起不来 {}：{error}", spec.program.display()))?;
    let pgid = child.id() as i32;
    let stdout_rx = child.stdout.take().map(|out| pump(out, spec.stdout_cap));
    let stderr_rx = child.stderr.take().map(|err| pump(err, spec.stderr_cap));

    let mut timed_out = false;
    let code;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                code = status.code();
                break;
            }
            Ok(None) => {}
            Err(error) => return Err(format!("等待进程失败：{error}")),
        }
        if started.elapsed() >= spec.timeout {
            timed_out = true;
            terminate(&mut child, pgid);
            code = None;
            break;
        }
        thread::sleep(POLL);
    }

    let (stdout, stdout_truncated) = stdout_rx
        .map(|rx| rx.recv_timeout(Duration::from_secs(2)))
        .and_then(Result::ok)
        .unwrap_or_default();
    let (stderr, stderr_truncated) = stderr_rx
        .map(|rx| rx.recv_timeout(Duration::from_secs(2)))
        .and_then(Result::ok)
        .unwrap_or_default();

    Ok(ExecOutput {
        code,
        stdout,
        stderr,
        timed_out,
        stdout_truncated,
        stderr_truncated,
        duration: started.elapsed(),
    })
}

fn terminate(child: &mut Child, pgid: i32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        thread::sleep(Duration::from_millis(30));
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sh(script: &str) -> ExecSpec {
        ExecSpec::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .timeout(Duration::from_secs(5))
    }

    #[test]
    fn captures_stdout_and_exit_code() {
        let out = run(&sh("printf 'hello'")).expect("跑起来");
        assert!(out.ok());
        assert_eq!(out.stdout_text(), "hello");
        assert_eq!(out.code, Some(0));
    }

    #[test]
    fn nonzero_exit_is_reported_not_err() {
        let out = run(&sh("printf 'bad' >&2; exit 3")).expect("跑起来");
        assert!(!out.ok());
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stderr_text().trim(), "bad");
        assert!(out.tail_text(80).contains("bad"));
    }

    #[test]
    fn timeout_kills_the_process() {
        let started = Instant::now();
        let out = run(&sh("sleep 5").timeout(Duration::from_millis(300))).expect("跑起来");
        assert!(out.timed_out);
        assert_eq!(out.code, None);
        assert!(started.elapsed() < Duration::from_secs(4), "应该被提前杀掉");
    }

    #[test]
    fn timeout_takes_down_the_whole_group() {
        // 子 shell 拉一个后台 sleep，父进程被 TERM 后孙子不该继续活着。
        let out = run(&sh("sleep 30 & echo $!; wait").timeout(Duration::from_millis(300)))
            .expect("跑起来");
        assert!(out.timed_out);
        let pid: i32 = out.stdout_text().trim().parse().expect("拿到孙进程 pid");
        thread::sleep(Duration::from_millis(300));
        // kill -0 探测：进程组被杀后这个 pid 不该还能被信号到。
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "孙进程 {pid} 还活着：进程组没杀干净");
    }

    #[test]
    fn output_is_capped_head_and_tail() {
        // 把上限压到 128 KiB：`seq` 的输出量受系统影响，不靠"猜多少行会超"。
        let mut spec = sh("seq 1 200000").timeout(Duration::from_secs(10));
        spec.stdout_cap = 128 * 1024;
        let out = run(&spec).expect("跑起来");
        assert!(out.stdout_truncated, "128 KiB 装不下 20 万行");
        let text = out.stdout_text();
        assert!(text.starts_with("1\n"), "头部要留：{}", &text[..20.min(text.len())]);
        assert!(text.contains("…（截断"));
        assert!(text.trim_end().ends_with("200000"), "尾部要留");
    }

    #[test]
    fn secrets_do_not_reach_the_child() {
        // 进程级 env 改完要恢复：同进程里还有别的测试在跑。
        let key = "PRISM_TEST_SECRET_TOKEN";
        std::env::set_var(key, "should-not-leak");
        let out = run(&sh(&format!("printf '%s' \"${{{key}}}\""))).expect("跑起来");
        std::env::remove_var(key);
        assert_eq!(out.stdout_text(), "");
    }

    #[test]
    fn arguments_are_not_shell_interpreted() {
        let dir = std::env::temp_dir().join(format!("prism-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let name = "a; echo pwned";
        std::fs::write(dir.join(name), b"x").expect("写文件");
        let out = run(
            &ExecSpec::new("/bin/echo")
                .arg(name)
                .cwd(Path::new(&dir))
                .timeout(Duration::from_secs(5)),
        )
        .expect("跑起来");
        assert_eq!(out.stdout_text().trim(), name);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_program_is_an_error() {
        let error = run(&ExecSpec::new("/definitely/not/here")).unwrap_err();
        assert!(error.contains("起不来"), "{error}");
    }
}
