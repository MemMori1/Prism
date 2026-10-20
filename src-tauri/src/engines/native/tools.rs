//! 原生引擎的内置工具。
//!
//! 六个就够覆盖日常：读、列、搜、写、改、跑命令。
//! 所有路径先做词法归一 + 前缀校验，禁止越出会话工作目录；命令有超时和输出上限。

use serde_json::{json, Value};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::Manager;

use crate::tasks::{SpawnRequest, TaskInfo, TasksState};

/// 单次工具输出的字符上限（超出截断，避免一轮就把上下文撑爆）。
const MAX_OUTPUT_CHARS: usize = 20_000;
/// 读文件默认行数。
const READ_LINES: usize = 2000;
/// 列目录的最大条目数。
const LIST_ENTRIES: usize = 300;
/// 搜索命中上限。
const GREP_HITS: usize = 120;
/// 搜索时跳过的大文件（字节）。
const GREP_MAX_BYTES: u64 = 1_500_000;
/// 跑命令时的轮询间隔：停止和超时都按这个粒度看。
const RUN_POLL: Duration = Duration::from_millis(40);
/// 单路管道最多留多少字节：读的人不能停手，但也不必无限往内存里攒
/// （展示时本来就要截断，这里只要留得比 MAX_OUTPUT_CHARS 富余即可）。
const PIPE_KEEP_BYTES: usize = 512 * 1024;
/// 遍历时永远跳过的目录。
const SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", "target", "dist", "build", ".next", ".nuxt", ".venv", "__pycache__",
    ".turbo", ".cache", "vendor",
];
/// 遍历时的文件数上限：到顶就在结论里说明，不能装成「都看过了」。
const WALK_MAX_FILES: usize = 4000;
/// 遍历的最大深度。按真实仓库的典型层级给：src-tauri/src/engines/native/*.rs
/// 已经在第 4 层，原来限 3 层会把一多半源码挡在外面（实测漏搜过）。
const WALK_MAX_DEPTH: usize = 6;

/// 遍历被上限截断的情况。
#[derive(Debug, Default, Clone, Copy)]
struct WalkCut {
    files: bool,
    depth: bool,
}

impl WalkCut {
    fn any(&self) -> bool {
        self.files || self.depth
    }

    /// 一句人话：这次只扫了一部分，结论要打折看。
    fn note(&self) -> Option<String> {
        if !self.any() {
            return None;
        }
        let mut reasons = Vec::new();
        if self.files {
            reasons.push(format!("文件数到顶（{WALK_MAX_FILES} 个）"));
        }
        if self.depth {
            reasons.push(format!("目录到顶（{WALK_MAX_DEPTH} 层）"));
        }
        Some(format!(
            "注意：这次扫描被截断了——{}，范围没扫全；把 path 指到更窄的目录再试。",
            reasons.join("、")
        ))
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub text: String,
    /// 失败时为错误说明（界面上工具卡标红）。
    pub error: Option<String>,
}

impl ToolOutcome {
    pub(crate) fn ok(text: impl Into<String>) -> Self {
        ToolOutcome {
            text: text.into(),
            error: None,
        }
    }

    pub fn fail(text: impl Into<String>) -> Self {
        let text = text.into();
        ToolOutcome {
            error: Some(text.clone()),
            text,
        }
    }
}

/// 发给模型的工具声明（OpenAI function schema）。
pub fn specs() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "读取工作目录里的一个文本文件，带行号。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "相对工作目录的路径" },
                        "offset": { "type": "integer", "description": "起始行（1 开始），默认 1" },
                        "limit": { "type": "integer", "description": "最多读多少行，默认 2000" }
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_files",
                "description": "列出目录下的文件，最多下探六层。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "相对路径，默认工作目录根" },
                        "pattern": { "type": "string", "description": "简单的通配过滤，如 *.rs" }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "在文件内容里搜子串，返回 文件:行号:内容。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "要搜的文本" },
                        "path": { "type": "string", "description": "搜索起点，默认工作目录根" },
                        "ignore_case": { "type": "boolean", "description": "忽略大小写" }
                    },
                    "required": ["pattern"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "写文件（新建或整体覆盖）。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "把文件里的 old_string 精确替换成 new_string；old_string 必须唯一，或显式 replace_all。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_string": { "type": "string", "description": "要替换的原文（含缩进）" },
                        "new_string": { "type": "string" },
                        "replace_all": { "type": "boolean" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "在工作目录里跑一条 shell 命令，返回退出码和输出。长命令（构建 / 全量测试 / dev server / 预计超过 ~30 秒）把 background 设成 true：立即返回任务 id，进程进「后台任务」坞继续跑，你不需要干等，之后用 task_status 看进度。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "timeout_ms": { "type": "integer", "description": "前台执行的超时毫秒，默认 120000（background=true 时不生效）" },
                        "background": { "type": "boolean", "description": "true = 转后台：立即返回，输出进任务坞 / 终端页，用 task_status 查" }
                    },
                    "required": ["command"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "task_status",
                "description": "查看本会话的后台任务（run_command background=true 起的，以及用户新建的终端任务）。不传 id：列出全部；传 id：返回该任务最近输出；stop=true：停止它。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "任务 id；省略则列出全部" },
                        "lines": { "type": "integer", "description": "看多少行尾部输出，默认 120，最多 400" },
                        "stop": { "type": "boolean", "description": "true = 停止这个任务（需要 id）" }
                    }
                }
            }
        }),
    ]
}

/// 工具执行时的运行时上下文：后台任务要 AppHandle 才能进注册表 / 发事件。
pub struct ToolRuntime<'a> {
    pub app: &'a tauri::AppHandle,
    /// 任务归属：任务坞里标「本会话」，task_status 只列这个会话的。
    pub session_id: &'a str,
}

/// 执行一次工具调用（无运行时上下文：单测走这条，后台任务不可用）。
/// 带一个「该停手了吗」的探针：跑命令时每轮都问一次，命中就连同命令拉起的
/// 子进程一起收掉（停止按钮不该等命令自己跑完）。
#[cfg_attr(not(test), allow(dead_code))]
pub fn execute_with_stop(
    directory: &str,
    name: &str,
    arguments: &str,
    should_stop: &dyn Fn() -> bool,
) -> ToolOutcome {
    dispatch(directory, name, arguments, should_stop, None)
}

/// 带运行时上下文执行：`run_command` 的 background 模式与 `task_status`，
/// 靠它接进 `TasksState`（和用户在终端页起的任务是同一条通道）。
pub fn execute_with_runtime(
    runtime: ToolRuntime<'_>,
    directory: &str,
    name: &str,
    arguments: &str,
    should_stop: &dyn Fn() -> bool,
) -> ToolOutcome {
    dispatch(directory, name, arguments, should_stop, Some(&runtime))
}

fn dispatch(
    directory: &str,
    name: &str,
    arguments: &str,
    should_stop: &dyn Fn() -> bool,
    runtime: Option<&ToolRuntime<'_>>,
) -> ToolOutcome {
    let args: Value = if arguments.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(arguments) {
            Ok(value) => value,
            Err(error) => return ToolOutcome::fail(format!("入参不是合法 JSON：{error}")),
        }
    };
    let root = Path::new(directory);
    match name {
        "read_file" => read_file(root, &args),
        "list_files" => list_files(root, &args),
        "grep" => grep(root, &args),
        "write_file" => write_file(root, &args),
        "edit_file" => edit_file(root, &args),
        "run_command" => run_command(root, &args, should_stop, runtime),
        "task_status" => task_status(&args, runtime),
        other => ToolOutcome::fail(format!("没有这个工具：{other}")),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).filter(|value| !value.is_empty())
}

fn int_arg(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn bool_arg(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// 归一化路径并确认没跑出工作目录。
fn resolve(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(root.to_path_buf());
    }
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    // 词法归一：处理 `..` / `.`，不做 IO（文件可能还不存在）。
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    let root_abs = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf());
    let base = normalized
        .ancestors()
        .find(|path| path.exists())
        .and_then(|path| path.canonicalize().ok());
    let inside = match base {
        Some(base) => base.starts_with(&root_abs) || base == root_abs,
        None => normalized.starts_with(&root_abs),
    };
    if !inside {
        return Err(format!("路径越出工作目录：{raw}"));
    }
    Ok(normalized)
}

fn read_file(root: &Path, args: &Value) -> ToolOutcome {
    let Some(path) = str_arg(args, "path") else {
        return ToolOutcome::fail("缺少 path");
    };
    let target = match resolve(root, path) {
        Ok(target) => target,
        Err(error) => return ToolOutcome::fail(error),
    };
    let Ok(text) = std::fs::read_to_string(&target) else {
        return ToolOutcome::fail(format!("读不到 {}", target.display()));
    };
    let offset = int_arg(args, "offset").unwrap_or(1).max(1) as usize;
    let limit = int_arg(args, "limit").unwrap_or(READ_LINES as u64).clamp(1, 5000) as usize;
    let mut out = String::new();
    for (index, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        out.push_str(&format!("{:>5}→{}\n", index + 1, line));
    }
    if out.is_empty() {
        return ToolOutcome::ok("（该范围没有内容）");
    }
    ToolOutcome::ok(clamp(&out))
}

fn list_files(root: &Path, args: &Value) -> ToolOutcome {
    let start = match str_arg(args, "path").map(|path| resolve(root, path)) {
        Some(Ok(path)) => path,
        Some(Err(error)) => return ToolOutcome::fail(error),
        None => root.to_path_buf(),
    };
    let pattern = str_arg(args, "pattern").map(str::to_string);
    let mut files = Vec::new();
    let cut = walk(&start, 0, &mut files);
    let note = cut
        .note()
        .map(|note| format!("{note}\n"))
        .unwrap_or_default();
    let mut out = String::new();
    let mut shown = 0;
    for file in files {
        let text = file.display().to_string();
        if let Some(pattern) = &pattern {
            let name = file.file_name().map(|v| v.to_string_lossy().to_string()).unwrap_or_default();
            if !wildcard_match(pattern, &name) {
                continue;
            }
        }
        if shown >= LIST_ENTRIES {
            out.push_str("…（还有更多，已截断）\n");
            break;
        }
        out.push_str(&format!("{text}\n"));
        shown += 1;
    }
    if out.is_empty() {
        return ToolOutcome::ok(format!("{note}（没有匹配的文件）"));
    }
    ToolOutcome::ok(clamp(&format!("{note}{out}")))
}

fn grep(root: &Path, args: &Value) -> ToolOutcome {
    let Some(pattern) = str_arg(args, "pattern") else {
        return ToolOutcome::fail("缺少 pattern");
    };
    let start = match str_arg(args, "path").map(|path| resolve(root, path)) {
        Some(Ok(path)) => path,
        Some(Err(error)) => return ToolOutcome::fail(error),
        None => root.to_path_buf(),
    };
    let ignore_case = bool_arg(args, "ignore_case");
    let needle = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let mut files = Vec::new();
    let cut = walk(&start, 0, &mut files);
    // 截断说明放在最前面：clamp 只保头，放后面会被截掉。
    let note = cut
        .note()
        .map(|note| format!("{note}\n"))
        .unwrap_or_default();
    let mut out = String::new();
    let mut hits = 0;
    for file in files {
        let Ok(meta) = std::fs::metadata(&file) else { continue };
        if meta.len() > GREP_MAX_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if text.contains('\u{0}') {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            let haystack = if ignore_case {
                line.to_lowercase()
            } else {
                line.to_string()
            };
            if !haystack.contains(&needle) {
                continue;
            }
            out.push_str(&format!(
                "{}:{}:{}\n",
                file.display(),
                index + 1,
                line.trim_end()
            ));
            hits += 1;
            if hits >= GREP_HITS {
                out.push_str("…（命中过多，已截断）\n");
                return ToolOutcome::ok(clamp(&format!("{note}{out}")));
            }
        }
    }
    if out.is_empty() {
        // 扫不全的时候「没有命中」是错误结论：必须把话说明白。
        return ToolOutcome::ok(format!("{note}（没有命中）"));
    }
    ToolOutcome::ok(clamp(&format!("{note}{out}")))
}

fn write_file(root: &Path, args: &Value) -> ToolOutcome {
    let Some(path) = str_arg(args, "path") else {
        return ToolOutcome::fail("缺少 path");
    };
    let Some(content) = args.get("content").and_then(Value::as_str) else {
        return ToolOutcome::fail("缺少 content");
    };
    let target = match resolve(root, path) {
        Ok(target) => target,
        Err(error) => return ToolOutcome::fail(error),
    };
    if let Some(parent) = target.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return ToolOutcome::fail(format!("创建目录失败：{error}"));
        }
    }
    match std::fs::write(&target, content) {
        Ok(()) => ToolOutcome::ok(format!(
            "已写入 {}（{} 行）",
            target.display(),
            content.lines().count()
        )),
        Err(error) => ToolOutcome::fail(format!("写入失败：{error}")),
    }
}

/// 文件与 old_string 的行尾对不上时，给出「照文件行尾换一遍」的版本。
///
/// 只处理两种明确的错配：文件 CRLF 而 old_string 只有 \n（read_file 之后
/// 照着抄的常态），以及反过来文件 LF 而 old_string 带着 \r。换过之后 new_string
/// 也一起换，免得改完一段就混出行尾来。
fn line_ending_variant(text: &str, old: &str, new: &str) -> Option<(String, String)> {
    let file_crlf = text.contains("\r\n");
    if file_crlf && old.contains('\n') && !old.contains('\r') {
        return Some((to_crlf(old), to_crlf(new)));
    }
    if !file_crlf && old.contains("\r\n") {
        return Some((old.replace("\r\n", "\n"), new.replace("\r\n", "\n")));
    }
    None
}

fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn edit_file(root: &Path, args: &Value) -> ToolOutcome {
    let Some(path) = str_arg(args, "path") else {
        return ToolOutcome::fail("缺少 path");
    };
    let Some(old) = args.get("old_string").and_then(Value::as_str) else {
        return ToolOutcome::fail("缺少 old_string");
    };
    let Some(new) = args.get("new_string").and_then(Value::as_str) else {
        return ToolOutcome::fail("缺少 new_string");
    };
    if old.is_empty() {
        return ToolOutcome::fail("old_string 不能为空（整体重写请用 write_file）");
    }
    let target = match resolve(root, path) {
        Ok(target) => target,
        Err(error) => return ToolOutcome::fail(error),
    };
    let Ok(text) = std::fs::read_to_string(&target) else {
        return ToolOutcome::fail(format!("读不到 {}", target.display()));
    };
    // read_file 只给行内容（看不到 \r），模型照着抄回来的 old_string 在 CRLF
    // 文件里就永远匹配不上——按文件行尾换一遍再试。
    let variant = line_ending_variant(&text, old, new);
    let (old, new) = match &variant {
        Some((variant_old, variant_new)) => (variant_old.as_str(), variant_new.as_str()),
        None => (old, new),
    };
    let count = text.matches(old).count();
    if count == 0 {
        // 行尾对不上是常见坑，失败时把话说明白：别让模型反复试错。
        let hint = if variant.is_some() {
            "；这个文件是 CRLF 行尾（read_file 里看不到 \\r），已按 \\r\\n 试过一次仍不匹配"
        } else {
            ""
        };
        return ToolOutcome::fail(format!(
            "没有找到与 old_string 完全一致的片段{hint}：请带上完整缩进与上下文行再试"
        ));
    }
    let replace_all = bool_arg(args, "replace_all");
    if count > 1 && !replace_all {
        return ToolOutcome::fail(format!(
            "old_string 出现了 {count} 次，请补足上下文让它唯一，或传 replace_all=true"
        ));
    }
    let updated = if replace_all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    match std::fs::write(&target, &updated) {
        Ok(()) => ToolOutcome::ok(format!(
            "已替换 {} 处：{}",
            if replace_all { count } else { 1 },
            target.display()
        )),
        Err(error) => ToolOutcome::fail(format!("写入失败：{error}")),
    }
}

fn run_command(
    root: &Path,
    args: &Value,
    should_stop: &dyn Fn() -> bool,
    runtime: Option<&ToolRuntime<'_>>,
) -> ToolOutcome {
    let Some(command) = str_arg(args, "command") else {
        return ToolOutcome::fail("缺少 command");
    };
    // 后台模式：进 TasksState（和用户在终端页起的任务是同一条通道），立即返回。
    // 长命令不再占住这一轮，也不需要 120s 超时；进度由 task_status 查。
    if bool_arg(args, "background") {
        return spawn_background_task(runtime, root, command);
    }
    let timeout = Duration::from_millis(
        int_arg(args, "timeout_ms")
            .unwrap_or(120_000)
            .clamp(1_000, 600_000),
    );
    let mut process = if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    };
    process
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 子进程自己领一个进程组：`sh -c` 拉起的孙子进程（cargo / rustc / npm …）
    // 只杀 sh 是带不走的，成组才能一起收掉。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return ToolOutcome::fail(format!("启动命令失败：{error}")),
    };
    // 两路管道都要有人同时读：写满一管（macOS 64KB）之后子进程会永远卡在 write 上，
    // 于是「命令话多」被误判成「命令超时」——cargo build 就是常客。
    let stdout_reader = child.stdout.take().map(spawn_pipe_reader);
    let stderr_reader = child.stderr.take().map(spawn_pipe_reader);

    enum Ending {
        Done,
        Stopped,
        TimedOut,
    }
    let started = Instant::now();
    let ending = loop {
        match child.try_wait() {
            Ok(Some(_)) => break Ending::Done,
            Ok(None) => {}
            Err(error) => {
                kill_tree(&mut child);
                return ToolOutcome::fail(format!("等待命令失败：{error}"));
            }
        }
        if should_stop() {
            kill_tree(&mut child);
            break Ending::Stopped;
        }
        if started.elapsed() > timeout {
            kill_tree(&mut child);
            break Ending::TimedOut;
        }
        std::thread::sleep(RUN_POLL);
    };
    // 杀过也要 wait：收掉僵尸，也让两个读线程读干净后退出。
    let status = child.wait().ok();
    let stdout = stdout_reader.map(join_pipe_reader).unwrap_or_default();
    let stderr = stderr_reader.map(join_pipe_reader).unwrap_or_default();

    let code = status.and_then(|value| value.code()).unwrap_or(-1);
    let mut text = match ending {
        Ending::Done => format!("exit={code}\n"),
        Ending::Stopped => format!("已被用户停止，命令已终止：{command}\nexit={code}\n"),
        Ending::TimedOut => format!(
            "命令超时（{} ms）已终止：{command}\nexit={code}\n",
            timeout.as_millis()
        ),
    };
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    if !stdout.trim().is_empty() {
        text.push_str("--- stdout ---\n");
        text.push_str(stdout.trim_end());
        text.push('\n');
    }
    if !stderr.trim().is_empty() {
        text.push_str("--- stderr ---\n");
        text.push_str(stderr.trim_end());
        text.push('\n');
    }
    let text = clamp(&text);
    let ok = matches!(ending, Ending::Done) && status.map(|value| value.success()).unwrap_or(false);
    if ok {
        ToolOutcome::ok(text)
    } else {
        ToolOutcome::fail(text)
    }
}

/// `run_command background=true`：起一个 agent 任务，立刻把 id 交还给模型。
fn spawn_background_task(
    runtime: Option<&ToolRuntime<'_>>,
    root: &Path,
    command: &str,
) -> ToolOutcome {
    let Some(runtime) = runtime else {
        return ToolOutcome::fail("当前上下文不支持后台任务（background 只能在本机会话里用）");
    };
    let tasks = runtime.app.state::<TasksState>();
    match tasks.spawn(
        runtime.app.clone(),
        SpawnRequest {
            cmd: command.to_string(),
            cwd: root.to_string_lossy().into_owned(),
            session_id: Some(runtime.session_id.to_string()),
            title: None,
            background: true,
            origin: "agent".to_string(),
        },
    ) {
        Ok(info) => ToolOutcome::ok(format!(
            "已在后台启动：{command}\n任务 id：{id}\n输出会实时进「后台任务」坞 / 文件侧栏的「终端」页；用 task_status（id: {id}）看进度，stop=true 停止。",
            id = info.id
        )),
        Err(error) => ToolOutcome::fail(format!("启动后台任务失败：{error}")),
    }
}

/// `task_status`：列本会话的任务 / 看某条尾部输出 / 停止。
fn task_status(args: &Value, runtime: Option<&ToolRuntime<'_>>) -> ToolOutcome {
    let Some(runtime) = runtime else {
        return ToolOutcome::fail("当前上下文不能查后台任务");
    };
    let tasks = runtime.app.state::<TasksState>();
    // 只认这个会话起的任务：别的会话的命令不该从工具里漏出来。
    let mine = |task: &TaskInfo| task.session_id.as_deref() == Some(runtime.session_id);

    if bool_arg(args, "stop") {
        let Some(id) = str_arg(args, "id") else {
            return ToolOutcome::fail("stop=true 需要同时给 id");
        };
        return match tasks.stop(id) {
            Ok(()) => ToolOutcome::ok(format!("已停止任务 {id}")),
            Err(error) => ToolOutcome::fail(error),
        };
    }

    if let Some(id) = str_arg(args, "id") {
        let Some(info) = tasks.list().into_iter().find(|task| task.id == id && mine(task)) else {
            return ToolOutcome::fail(format!("找不到任务 {id}（只能看本会话起的任务）"));
        };
        let lines = int_arg(args, "lines").unwrap_or(120).clamp(1, 400) as usize;
        let start = info.tail.len().saturating_sub(lines);
        let body = info.tail[start..].join("\n");
        let body = if body.trim().is_empty() {
            "（还没有输出）".to_string()
        } else {
            body
        };
        return ToolOutcome::ok(clamp(&format!(
            "任务 {}\n命令：{}\n状态：{}{}\n目录：{}\n--- 最近 {lines} 行输出 ---\n{body}",
            info.id,
            info.cmd,
            state_label(&info.state),
            exit_suffix(&info),
            info.cwd
        )));
    }

    let list: Vec<TaskInfo> = tasks.list().into_iter().filter(mine).collect();
    if list.is_empty() {
        return ToolOutcome::ok("这个会话还没有后台任务。");
    }
    let mut text = String::from("本会话的后台任务：\n");
    for task in list {
        text.push_str(&format!(
            "- {} · {} · {}{}\n  命令：{}\n",
            task.id,
            state_label(&task.state),
            elapsed_label(&task),
            exit_suffix(&task),
            task.cmd
        ));
    }
    ToolOutcome::ok(clamp(&text))
}

fn state_label(state: &str) -> &'static str {
    match state {
        "running" => "运行中",
        "done" => "已完成",
        "failed" => "失败",
        "stopped" => "已停止",
        _ => "未知",
    }
}

fn elapsed_label(task: &TaskInfo) -> String {
    let end = task.ended_at.unwrap_or_else(now_ms);
    let seconds = ((end - task.started_at).max(0) / 1000) as u64;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn exit_suffix(task: &TaskInfo) -> String {
    match task.state.as_str() {
        "running" => String::new(),
        "stopped" => "（已停止）".to_string(),
        _ => match task.exit_code {
            Some(code) => format!("（exit {code}）"),
            None => String::new(),
        },
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

/// 一路管道交给一个线程读到结束：读的人不读，子进程就会卡在 write 上。
fn spawn_pipe_reader(pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || drain_pipe(pipe))
}

fn join_pipe_reader(handle: std::thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

/// 读到结束；最多留 PIPE_KEEP_BYTES——展示时本来就要截断，多余的只是占内存。
fn drain_pipe(mut pipe: impl Read) -> Vec<u8> {
    let mut kept: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let room = PIPE_KEEP_BYTES.saturating_sub(kept.len());
                if room == 0 {
                    continue;
                }
                let take = count.min(room);
                kept.extend_from_slice(&buffer[..take]);
            }
            Err(_) => break,
        }
    }
    kept
}

/// 收掉整组子进程：先 SIGTERM 给个收尾机会，赖着不走再 SIGKILL。
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // spawn 时设了 process_group(0)：组号＝子进程 pid，负数表示「整组」。
        let group = child.id() as libc::pid_t;
        unsafe {
            libc::kill(-group, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            // 已经退出就不再发信号：僵尸被收掉后 pid 可能被系统复用。
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// 递归收集文件（有深度与跳过规则，不跟符号链接）。
/// 返回值说明这次遍历有没有被上限截断——截断过就必须在结论里讲清楚。
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> WalkCut {
    let mut cut = WalkCut::default();
    if depth > WALK_MAX_DEPTH {
        cut.depth = true;
        return cut;
    }
    if out.len() >= WALK_MAX_FILES {
        cut.files = true;
        return cut;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return cut;
    };
    let mut items: Vec<_> = entries.filter_map(Result::ok).collect();
    items.sort_by_key(|entry| entry.file_name());
    for entry in items {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(file_type) = entry.file_type() else { continue };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let child = walk(&path, depth + 1, out);
            cut.files |= child.files;
            cut.depth |= child.depth;
        } else if file_type.is_file() {
            if out.len() >= WALK_MAX_FILES {
                cut.files = true;
                break;
            }
            out.push(path);
        }
    }
    cut
}

/// 极简通配：只支持 `*` 与 `?`。
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(star_index) = star {
            pi = star_index + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

fn clamp(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n…（输出过长，已截断）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// 大多数用例不关心停止：固定传 false。
    fn execute(root: &str, name: &str, arguments: &str) -> ToolOutcome {
        execute_with_stop(root, name, arguments, &|| false)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("prism-native-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("建临时目录");
        path
    }

    #[test]
    fn specs_cover_builtin_tools() {
        let names: Vec<String> = specs()
            .iter()
            .filter_map(|spec| spec["function"]["name"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            names,
            vec![
                "read_file",
                "list_files",
                "grep",
                "write_file",
                "edit_file",
                "run_command",
                "task_status"
            ]
        );
    }

    #[test]
    fn read_write_round_trip_has_line_numbers() {
        let dir = temp_dir("rw");
        let root = dir.to_string_lossy().to_string();
        write_file(&dir, &json!({ "path": "a.txt", "content": "第一行\n第二行\n" }));
        let outcome = execute(&root, "read_file", r#"{"path":"a.txt"}"#);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert!(outcome.text.contains("1→第一行"), "{}", outcome.text);
        assert!(outcome.text.contains("2→第二行"), "{}", outcome.text);
    }

    #[test]
    fn edit_requires_unique_match() {
        let dir = temp_dir("edit");
        let root = dir.to_string_lossy().to_string();
        execute(
            &root,
            "write_file",
            r#"{"path":"b.txt","content":"x\nx\n"}"#,
        );
        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"b.txt","old_string":"x","new_string":"y"}"#,
        );
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("2 次"), "{}", outcome.text);

        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"b.txt","old_string":"x","new_string":"y","replace_all":true}"#,
        );
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "y\ny\n");
    }

    /// CRLF 文件：read_file 里看不到 \r，模型照抄回来的 old_string 也必须能改。
    #[test]
    fn edit_file_accepts_lf_old_string_in_crlf_file() {
        let dir = temp_dir("crlf");
        let root = dir.to_string_lossy().to_string();
        write_file(
            &dir,
            &json!({ "path": "c.txt", "content": "alpha\r\nbeta\r\ngamma\r\n" }),
        );
        // 读出来是这个样子：看不到 \r，模型也只能照着这样子抄。
        let read = execute(&root, "read_file", r#"{"path":"c.txt"}"#);
        assert!(read.text.contains("1→alpha"), "{}", read.text);

        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"c.txt","old_string":"alpha\nbeta","new_string":"one\ntwo"}"#,
        );
        assert!(outcome.error.is_none(), "{}", outcome.text);
        // 改完还是 CRLF：不能改一段就混出行尾来。
        assert_eq!(
            fs::read_to_string(dir.join("c.txt")).unwrap(),
            "one\r\ntwo\r\ngamma\r\n"
        );
    }

    /// 反过来也要认：文件是 LF，old_string 却带着 \r。
    #[test]
    fn edit_file_accepts_crlf_old_string_in_lf_file() {
        let dir = temp_dir("lf");
        let root = dir.to_string_lossy().to_string();
        write_file(
            &dir,
            &json!({ "path": "d.txt", "content": "alpha\nbeta\n" }),
        );
        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"d.txt","old_string":"alpha\r\nbeta","new_string":"one\r\ntwo"}"#,
        );
        assert!(outcome.error.is_none(), "{}", outcome.text);
        assert_eq!(
            fs::read_to_string(dir.join("d.txt")).unwrap(),
            "one\ntwo\n"
        );
    }

    /// 真的找不到时要把行尾这件事说出来，别让模型反复试错。
    #[test]
    fn edit_file_failure_mentions_crlf() {
        let dir = temp_dir("crlfmiss");
        let root = dir.to_string_lossy().to_string();
        write_file(
            &dir,
            &json!({ "path": "e.txt", "content": "alpha\r\nbeta\r\n" }),
        );
        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"e.txt","old_string":"alpha\ngamma","new_string":"x"}"#,
        );
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("CRLF"), "{}", outcome.text);
    }

    #[test]
    fn paths_outside_workspace_are_rejected() {
        let dir = temp_dir("guard");
        let root = dir.to_string_lossy().to_string();
        let outcome = execute(&root, "read_file", r#"{"path":"../../etc/hosts"}"#);
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("越出"), "{}", outcome.text);
    }

    fn sample_task(state: &str, exit_code: Option<i32>) -> TaskInfo {
        TaskInfo {
            id: "t1".to_string(),
            cmd: "pnpm build".to_string(),
            cwd: "/tmp".to_string(),
            state: state.to_string(),
            started_at: 1_000,
            ended_at: if state == "running" { None } else { Some(73_000) },
            exit_code,
            session_id: Some("s1".to_string()),
            title: None,
            background: true,
            origin: "agent".to_string(),
            tail: vec![],
        }
    }

    #[test]
    fn task_status_labels_are_readable() {
        assert_eq!(state_label("running"), "运行中");
        assert_eq!(state_label("done"), "已完成");
        assert_eq!(state_label("failed"), "失败");
        assert_eq!(state_label("stopped"), "已停止");

        assert_eq!(exit_suffix(&sample_task("running", None)), "");
        assert_eq!(exit_suffix(&sample_task("done", Some(0))), "（exit 0）");
        assert_eq!(exit_suffix(&sample_task("failed", Some(128))), "（exit 128）");
        assert_eq!(exit_suffix(&sample_task("stopped", None)), "（已停止）");

        assert_eq!(elapsed_label(&sample_task("done", Some(0))), "01:12");
        // 跑着的按 now 算，别把秒边界写死。
        let mut running = sample_task("running", None);
        running.started_at = now_ms() - 72_000;
        let label = elapsed_label(&running);
        assert!(label == "01:12" || label == "01:13", "{label}");
    }

    /// 后台模式必须在有运行时上下文时才能用：单测里没有，要明确报错而不是悄悄前台跑。
    #[test]
    fn background_command_needs_runtime() {
        let outcome = execute(&std::env::temp_dir().to_string_lossy(), "run_command", r#"{"command":"echo hi","background":true}"#);
        assert!(outcome.error.is_some());
        assert!(outcome.error.unwrap().contains("不支持后台任务"));
    }

    #[test]
    fn task_status_needs_runtime() {
        let outcome = execute(&std::env::temp_dir().to_string_lossy(), "task_status", "{}");
        assert!(outcome.error.is_some());
    }

    /// 声明里必须有 background 开关和 task_status，否则模型根本不知道这条通道。
    #[test]
    fn specs_expose_background_and_task_status() {
        let specs = specs();
        let run = specs
            .iter()
            .find(|spec| spec["function"]["name"] == "run_command")
            .expect("run_command 声明");
        assert!(run["function"]["parameters"]["properties"]["background"].is_object());
        assert!(specs
            .iter()
            .any(|spec| spec["function"]["name"] == "task_status"));
    }

    #[test]
    fn run_command_reports_exit_code() {
        let dir = temp_dir("cmd");
        let root = dir.to_string_lossy().to_string();
        let outcome = execute(&root, "run_command", r#"{"command":"echo hi"}"#);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert!(outcome.text.contains("exit=0"), "{}", outcome.text);
        assert!(outcome.text.contains("hi"), "{}", outcome.text);
    }

    /// 输出超过管道缓冲（macOS 64KB）不能假性超时：读的人必须同时在读。
    #[test]
    fn run_command_large_output_is_not_a_timeout() {
        let dir = temp_dir("cmdout");
        let root = dir.to_string_lossy().to_string();
        let outcome = execute(
            &root,
            "run_command",
            r#"{"command":"yes x | head -c 300000","timeout_ms":10000}"#,
        );
        assert!(outcome.error.is_none(), "{}", outcome.text);
        assert!(outcome.text.contains("exit=0"), "{}", outcome.text);
    }

    /// 超大输出也不能把内存吃干：展示本来就要截断，读的时候留前一段就够。
    #[test]
    fn run_command_huge_output_is_capped() {
        let dir = temp_dir("cmdhuge");
        let root = dir.to_string_lossy().to_string();
        let started = Instant::now();
        let outcome = execute(
            &root,
            "run_command",
            r#"{"command":"yes x | head -c 4000000","timeout_ms":20000}"#,
        );
        assert!(outcome.error.is_none(), "{}", outcome.text);
        assert!(outcome.text.contains("已截断"), "{}", outcome.text.len());
        assert!(started.elapsed() < Duration::from_secs(20), "不该等到超时");
    }

    #[test]
    fn run_command_timeout_kills_slow_command() {
        let dir = temp_dir("cmdtimeout");
        let root = dir.to_string_lossy().to_string();
        let started = Instant::now();
        let outcome = execute(
            &root,
            "run_command",
            r#"{"command":"sleep 30","timeout_ms":1000}"#,
        );
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("命令超时"), "{}", outcome.text);
        assert!(started.elapsed() < Duration::from_secs(10), "超时要及时收手");
    }

    /// 停止按钮要能打断正在跑的命令（不是等命令自己跑完）。
    #[test]
    fn run_command_stop_kills_running_command() {
        let dir = temp_dir("cmdstop");
        let root = dir.to_string_lossy().to_string();
        let flag = Arc::new(AtomicBool::new(false));
        let flip = Arc::clone(&flag);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            flip.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let outcome = execute_with_stop(
            &root,
            "run_command",
            r#"{"command":"sleep 30","timeout_ms":60000}"#,
            &|| flag.load(Ordering::SeqCst),
        );
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("已被用户停止"), "{}", outcome.text);
        assert!(started.elapsed() < Duration::from_secs(5), "停止要立刻生效");
    }

    /// 命令拉起的孙子进程要一起收掉：只杀 sh 的话后台的活还会继续干。
    #[test]
    fn run_command_stop_takes_down_child_processes() {
        let dir = temp_dir("cmdtree");
        let root = dir.to_string_lossy().to_string();
        let flag = Arc::new(AtomicBool::new(false));
        let flip = Arc::clone(&flag);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            flip.store(true, Ordering::SeqCst);
        });
        // 后台那支 1.5 秒后会写下 marker；它要是活着，说明只杀了 sh。
        let outcome = execute_with_stop(
            &root,
            "run_command",
            r#"{"command":"(sleep 1.5; touch marker) & sleep 30","timeout_ms":60000}"#,
            &|| flag.load(Ordering::SeqCst),
        );
        assert!(outcome.text.contains("已被用户停止"), "{}", outcome.text);
        std::thread::sleep(Duration::from_millis(1800));
        assert!(!dir.join("marker").exists(), "孙子进程没被收掉");
    }

    /// 真实仓库的形状：文件在第 4 层（src-tauri/src/engines/native/*.rs）。
    /// 原来深度限 3 层，从根上搜不到、还回一句「没有命中」。
    #[test]
    fn grep_reaches_deep_repo_layout() {
        let dir = temp_dir("deepgrep");
        let root = dir.to_string_lossy().to_string();
        fs::create_dir_all(dir.join("src-tauri/src/engines/native")).unwrap();
        fs::write(dir.join("src-tauri/src/engines/native/hit.txt"), "needle\n").unwrap();
        let outcome = execute(&root, "grep", r#"{"pattern":"needle"}"#);
        assert!(outcome.text.contains("hit.txt"), "{}", outcome.text);
        assert!(
            !outcome.text.contains("截断"),
            "层数够，不该有截断提示：{}",
            outcome.text
        );
    }

    /// 真超上限时结论里必须说出来：不能回一句「没有命中」了事。
    #[test]
    fn grep_says_when_scan_was_cut() {
        let dir = temp_dir("cutgrep");
        let root = dir.to_string_lossy().to_string();
        fs::create_dir_all(dir.join("a/b/c/d/e/f/g")).unwrap();
        fs::write(dir.join("a/b/c/d/e/f/g/needle.txt"), "needle\n").unwrap();
        let outcome = execute(&root, "grep", r#"{"pattern":"needle"}"#);
        assert!(outcome.text.contains("没有命中"), "{}", outcome.text);
        assert!(outcome.text.contains("截断"), "{}", outcome.text);
        assert!(outcome.text.contains("path"), "要说清怎么缩小范围：{}", outcome.text);
    }

    /// 文件数到顶也一样：只能保证「扫过的那部分」。
    #[test]
    fn grep_says_when_file_cap_hit() {
        let dir = temp_dir("capfiles");
        let root = dir.to_string_lossy().to_string();
        for index in 0..(WALK_MAX_FILES + 5) {
            fs::write(dir.join(format!("f{index:05}.txt")), "x\n").unwrap();
        }
        let outcome = execute(&root, "grep", r#"{"pattern":"needle-nowhere"}"#);
        assert!(outcome.text.contains("没有命中"), "{}", outcome.text);
        assert!(outcome.text.contains("文件数到顶"), "{}", outcome.text);

        let listed = execute(&root, "list_files", "{}");
        assert!(listed.text.contains("文件数到顶"), "{}", listed.text);
    }

    #[test]
    fn wildcard_matches_simple_patterns() {
        assert!(wildcard_match("*.rs", "main.rs"));
        assert!(!wildcard_match("*.rs", "main.ts"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(wildcard_match("*", "anything"));
    }
}
