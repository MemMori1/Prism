//! 后台任务：会话之外独立跑的进程（构建 / 测试 / 脚本 / 长命令）。
//!
//! 取舍：
//! - 生命周期跟着 Prism 进程走（应用退出全部收掉），不做持久化——命令进程本来就
//!   不属于任何引擎的历史，重启后「复活」一条已经死掉的进程没有意义。
//! - 每个任务自领进程组（`process_group(0)`），停止时整组 SIGTERM → 900ms → SIGKILL，
//!   这样 `pnpm build` 拉起的 vite / cargo 孙子进程也能一起收掉。
//! - 输出逐行广播（`task-output`），同时留最近 [`TAIL_LINES`] 行在注册表里，
//!   前端刷新 / 重开面板时能直接拿回上下文。
//! - 状态变化广播整条 [`TaskInfo`]（`task-state`），删除广播 id（`task-removed`）。
//!   前端是副本：事件到达前先落的行不会丢（见 `taskModel.mergeTask`）。

use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

/// 每个任务保留的输出行数（够看尾部，又不至于把内存吃满）。
pub const TAIL_LINES: usize = 400;
/// 单行截断：有的工具会把整个 JSON / 源码打进一行。
const MAX_LINE_CHARS: usize = 4_000;
/// SIGTERM 之后等多久补 SIGKILL。
const KILL_GRACE_MS: u64 = 900;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInfo {
    pub id: String,
    pub cmd: String,
    pub cwd: String,
    /// running | done | failed | stopped
    pub state: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub exit_code: Option<i32>,
    /// 从哪个会话起的（任务坞里标「本会话 / 别的会话」）。
    pub session_id: Option<String>,
    pub title: Option<String>,
    /// false = 前台终端任务：只在终端页显示，不进任务坞。
    pub background: bool,
    /// user | agent：agent 起的命令只在「还在跑」时进坞，收口后留给终端页。
    pub origin: String,
    pub tail: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskOutput {
    pub id: String,
    pub line: String,
    /// stdout | stderr
    pub stream: String,
}

/// 起一条任务的入参（参数多，打包成结构体，别让调用方数位置）。
pub struct SpawnRequest {
    pub cmd: String,
    pub cwd: String,
    pub session_id: Option<String>,
    pub title: Option<String>,
    /// false = 只在终端页显示，不进任务坞。
    pub background: bool,
    /// user | agent
    pub origin: String,
}

struct Entry {
    info: TaskInfo,
    /// 进程组号＝子进程 pid（spawn 时 process_group(0)）。
    pgid: i32,
    /// 用户按过停止：收口时按 stopped 记，而不是 failed。
    stop_requested: Arc<AtomicBool>,
}

/// 事件出口：生产用 AppHandle（emit 到前端），测试用内存 sink。
/// 抽出这一层是为了能在没有 Tauri AppHandle 的单测里跑真实进程。
pub trait TaskSink: Clone + Send + 'static {
    fn output(&self, payload: TaskOutput);
    fn state(&self, info: TaskInfo);
}

impl TaskSink for AppHandle {
    fn output(&self, payload: TaskOutput) {
        let _ = self.emit("task-output", payload);
    }
    fn state(&self, info: TaskInfo) {
        let _ = self.emit("task-state", info);
    }
}

pub struct TasksState {
    tasks: Arc<Mutex<HashMap<String, Entry>>>,
    seq: AtomicU64,
}

impl TasksState {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            seq: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.tasks.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// 起一个后台任务；返回刚写入注册表的快照。
    pub fn spawn<S: TaskSink>(&self, sink: S, request: SpawnRequest) -> Result<TaskInfo, String> {
        let SpawnRequest {
            cmd,
            cwd,
            session_id,
            title,
            background,
            origin,
        } = request;
        let cmd = cmd.trim().to_string();
        if cmd.is_empty() {
            return Err("命令不能为空".to_string());
        }
        let dir = Path::new(&cwd);
        if !dir.is_dir() {
            return Err(format!("工作目录不存在：{cwd}"));
        }

        let mut command = shell(&cmd);
        command
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = command
            .spawn()
            .map_err(|error| format!("启动命令失败：{error}"))?;
        let pid = child.id();
        let started_at = now_ms();
        let id = format!(
            "task-{started_at}-{}",
            self.seq.fetch_add(1, Ordering::Relaxed)
        );
        let info = TaskInfo {
            id: id.clone(),
            cmd: cmd.clone(),
            cwd,
            state: "running".to_string(),
            started_at,
            ended_at: None,
            exit_code: None,
            session_id,
            title,
            background,
            origin,
            tail: vec![format!("$ {cmd}")],
        };
        let stop_requested = Arc::new(AtomicBool::new(false));
        self.lock().insert(
            id.clone(),
            Entry {
                info: info.clone(),
                pgid: pid as i32,
                stop_requested: stop_requested.clone(),
            },
        );
        // 先广播一次「已创建」：agent 起任务时前端拿不到 task_spawn 的返回值，
        // 只靠 `task-output` 的话找不到这条任务，第一行就被丢掉、坞里也不显示。
        sink.state(info.clone());

        if let Some(pipe) = child.stdout.take() {
            spawn_reader(sink.clone(), self.tasks.clone(), id.clone(), "stdout", pipe);
        }
        if let Some(pipe) = child.stderr.take() {
            spawn_reader(sink.clone(), self.tasks.clone(), id.clone(), "stderr", pipe);
        }
        spawn_waiter(sink, self.tasks.clone(), id, child, stop_requested);

        Ok(info)
    }

    /// 全部任务，新起的在前。
    pub fn list(&self) -> Vec<TaskInfo> {
        let mut list: Vec<TaskInfo> = self.lock().values().map(|entry| entry.info.clone()).collect();
        list.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        list
    }

    /// 停止：整组 SIGTERM，宽限期后还活着再 SIGKILL。
    pub fn stop(&self, id: &str) -> Result<(), String> {
        let (pgid, flag) = {
            let map = self.lock();
            let entry = map.get(id).ok_or_else(|| "任务不存在".to_string())?;
            if entry.info.state != "running" {
                return Err("任务已经结束".to_string());
            }
            (entry.pgid, entry.stop_requested.clone())
        };
        flag.store(true, Ordering::Relaxed);
        signal_group(pgid, Sig::Term);

        let tasks = self.tasks.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(KILL_GRACE_MS));
            let pgid = {
                let map = tasks.lock().unwrap_or_else(|error| error.into_inner());
                match map.get(&id) {
                    Some(entry) if entry.info.state == "running" => entry.pgid,
                    _ => 0,
                }
            };
            if pgid > 0 {
                signal_group(pgid, Sig::Kill);
            }
        });
        Ok(())
    }

    /// 移出列表：只允许已结束的任务。
    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut map = self.lock();
        let entry = map.get(id).ok_or_else(|| "任务不存在".to_string())?;
        if entry.info.state == "running" {
            return Err("任务还在跑，先停止再关闭".to_string());
        }
        map.remove(id);
        Ok(())
    }

    /// 应用退出：把所有还在跑的进程组收掉，别留孤儿。
    pub fn shutdown(&self) {
        let running = |state: &TasksState| -> Vec<i32> {
            state
                .lock()
                .values()
                .filter(|entry| entry.info.state == "running")
                .map(|entry| entry.pgid)
                .collect()
        };
        let groups = running(self);
        for group in groups {
            signal_group(group, Sig::Term);
        }
        if !running(self).is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(120));
            for group in running(self) {
                signal_group(group, Sig::Kill);
            }
        }
    }
}

impl Default for TasksState {
    fn default() -> Self {
        Self::new()
    }
}

/// 登录 shell 跑命令：Finder 启动的 App PATH 很干净，补两条常见 bin 目录。
fn shell(cmd: &str) -> Command {
    if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.arg("/C").arg(cmd);
        return command;
    }
    let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut command = Command::new(shell_path);
    command.arg("-lc").arg(cmd);
    let path = std::env::var("PATH").unwrap_or_default();
    command.env(
        "PATH",
        format!("/opt/homebrew/bin:/usr/local/bin:{path}"),
    );
    command
}

fn spawn_reader<S: TaskSink, R: Read + Send + 'static>(
    sink: S,
    tasks: Arc<Mutex<HashMap<String, Entry>>>,
    id: String,
    stream: &'static str,
    pipe: R,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut buffer: Vec<u8> = Vec::new();
        loop {
            buffer.clear();
            // 按字节读到换行再用 lossy 解码：工具偶尔吐非 UTF-8，
            // `lines()` 会直接中断整条流，宁可换几个问号也别丢后面的输出。
            match reader.read_until(b'\n', &mut buffer) {
                Ok(0) => break,
                Ok(_) => {
                    let raw = String::from_utf8_lossy(&buffer);
                    let line = clip_line(raw.trim_end_matches(['\n', '\r']));
                    {
                        let mut map = tasks.lock().unwrap_or_else(|error| error.into_inner());
                        if let Some(entry) = map.get_mut(&id) {
                            push_tail(&mut entry.info.tail, line.clone());
                        }
                    }
                    sink.output(TaskOutput {
                        id: id.clone(),
                        line,
                        stream: stream.to_string(),
                    });
                }
                Err(_) => break,
            }
        }
    });
}

fn spawn_waiter<S: TaskSink>(
    sink: S,
    tasks: Arc<Mutex<HashMap<String, Entry>>>,
    id: String,
    mut child: Child,
    stop_requested: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let status = child.wait();
        let (code, success) = match &status {
            Ok(status) => (status.code(), status.success()),
            Err(_) => (None, false),
        };
        let state = final_state(stop_requested.load(Ordering::Relaxed), success);
        let snapshot = {
            let mut map = tasks.lock().unwrap_or_else(|error| error.into_inner());
            map.get_mut(&id).map(|entry| {
                entry.info.state = state.to_string();
                entry.info.ended_at = Some(now_ms());
                entry.info.exit_code = code;
                entry.info.clone()
            })
        };
        if let Some(info) = snapshot {
            sink.state(info);
        }
    });
}

fn final_state(stopped: bool, success: bool) -> &'static str {
    if stopped {
        "stopped"
    } else if success {
        "done"
    } else {
        "failed"
    }
}

fn push_tail(tail: &mut Vec<String>, line: String) {
    if tail.len() >= TAIL_LINES {
        tail.remove(0);
    }
    tail.push(line);
}

fn clip_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let mut clipped: String = line.chars().take(MAX_LINE_CHARS).collect();
    clipped.push_str(" …");
    clipped
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Copy)]
enum Sig {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_group(pgid: i32, sig: Sig) {
    if pgid <= 0 {
        return;
    }
    let raw = match sig {
        Sig::Term => libc::SIGTERM,
        Sig::Kill => libc::SIGKILL,
    };
    unsafe {
        libc::kill(-pgid, raw);
    }
}

#[cfg(not(unix))]
fn signal_group(_pgid: i32, _sig: Sig) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// 事件收进内存，断言时直接看。
    #[derive(Clone, Default)]
    struct TestSink {
        outputs: Arc<Mutex<Vec<TaskOutput>>>,
        states: Arc<Mutex<Vec<TaskInfo>>>,
    }

    impl TaskSink for TestSink {
        fn output(&self, payload: TaskOutput) {
            self.outputs.lock().unwrap().push(payload);
        }
        fn state(&self, info: TaskInfo) {
            self.states.lock().unwrap().push(info);
        }
    }

    fn wait_until(mut check: impl FnMut() -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if check() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("等不到 {what}");
    }

    #[test]
    fn spawn_streams_output_and_records_exit_code() {
        let tasks = TasksState::new();
        let sink = TestSink::default();
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let info = tasks
            .spawn(
                sink.clone(),
                SpawnRequest {
                    cmd: "printf 'alpha\\nbeta\\n'; printf 'oops\\n' >&2; exit 3".to_string(),
                    cwd,
                    session_id: Some("s1".to_string()),
                    title: None,
                    background: true,
                    origin: "user".to_string(),
                },
            )
            .unwrap();
        assert_eq!(info.origin, "user");

        // 第一条状态事件必须是「已创建 / 运行中」——前端靠它把 agent 任务画进坞里。
        {
            let states = sink.states.lock().unwrap();
            let first = states.first().expect("创建时就该广播一次状态");
            assert_eq!(first.id, info.id);
            assert_eq!(first.state, "running");
        }
        wait_until(
            || {
                sink.states
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|item| item.id == info.id && item.state != "running")
            },
            "任务收口",
        );

        let final_info = tasks
            .list()
            .into_iter()
            .find(|item| item.id == info.id)
            .expect("任务还在注册表里");
        assert_eq!(final_info.state, "failed");
        assert_eq!(final_info.exit_code, Some(3));
        assert!(final_info.ended_at.is_some());
        assert!(final_info.tail.iter().any(|line| line.contains("alpha")));

        let outputs = sink.outputs.lock().unwrap();
        assert!(outputs
            .iter()
            .any(|item| item.stream == "stderr" && item.line.contains("oops")));
        assert!(outputs
            .iter()
            .any(|item| item.stream == "stdout" && item.line.contains("beta")));
    }

    #[test]
    fn stop_marks_task_stopped_and_kills_the_group() {
        let tasks = TasksState::new();
        let sink = TestSink::default();
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let info = tasks
            .spawn(
                sink.clone(),
                SpawnRequest {
                    cmd: "sleep 5".to_string(),
                    cwd,
                    session_id: None,
                    title: None,
                    background: false,
                    origin: "agent".to_string(),
                },
            )
            .unwrap();

        assert!(tasks.remove(&info.id).is_err(), "运行中不能直接移出");
        tasks.stop(&info.id).unwrap();

        wait_until(
            || {
                tasks
                    .list()
                    .into_iter()
                    .any(|item| item.id == info.id && item.state == "stopped")
            },
            "停止后收口",
        );

        let stopped = tasks
            .list()
            .into_iter()
            .find(|item| item.id == info.id)
            .unwrap();
        assert_eq!(stopped.state, "stopped");
        assert!(tasks.stop(&info.id).is_err(), "已经结束的任务不用再停");
        tasks.remove(&info.id).unwrap();
        assert!(tasks.list().is_empty());
    }

    #[test]
    fn final_state_prefers_stopped_over_failure() {
        assert_eq!(final_state(true, false), "stopped");
        assert_eq!(final_state(true, true), "stopped");
        assert_eq!(final_state(false, true), "done");
        assert_eq!(final_state(false, false), "failed");
    }

    #[test]
    fn tail_keeps_latest_lines() {
        let mut tail: Vec<String> = Vec::new();
        for index in 0..(TAIL_LINES + 5) {
            push_tail(&mut tail, format!("line-{index}"));
        }
        assert_eq!(tail.len(), TAIL_LINES);
        assert_eq!(tail.first().unwrap(), "line-5");
        assert_eq!(tail.last().unwrap(), &format!("line-{}", TAIL_LINES + 4));
    }

    #[test]
    fn long_lines_are_clipped() {
        let line = "x".repeat(MAX_LINE_CHARS + 40);
        let clipped = clip_line(&line);
        assert_eq!(clipped.chars().count(), MAX_LINE_CHARS + 2);
        assert!(clipped.ends_with('…'));
        assert_eq!(clip_line("short"), "short");
    }
}
