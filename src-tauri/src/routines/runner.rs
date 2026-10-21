//! 定时任务执行器：走引擎运行轴（新建会话 → 发提示词 → 等终态），
//! 记执行日志、更新定义状态，并广播 `routine-event` 供前端通知 / 刷新。
//!
//! 互斥：调度触发与"立即运行"共用 `RoutinesState.running` 集合（进程内）；
//! 进程退出即释放，无需文件锁（与 launchd 方案不同，见 ROADMAP M11 取舍）。
//! 权限：运行中若引擎请求审批，广播 permission 事件（用户点通知即可去处理）。

use serde::Serialize;
use serde_json::Value;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use super::scheduler::RoutinesState;
use super::store::{self, RoutineDefinition, RoutineLogEntry};
use crate::engines::engine::{Engine, EngineRegistry};
use crate::engines::opencode::search::prism_dir;

/// 单次运行的兜底时长；超时先打断再按 timeout 收口。
const RUN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// 例行任务的工作区（数据主权：不动用户项目目录）。
const WORKSPACE_DIR: &str = "routine-workspace";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineEventPayload {
    /// done | failed | permission
    pub kind: String,
    pub routine_id: String,
    pub name: String,
    pub engine: String,
    pub directory: String,
    pub session_id: Option<String>,
    pub started_at: i64,
    pub status: Option<String>,
    pub error: Option<String>,
    pub request_id: Option<String>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

/// 例行工作区目录（`~/.prism/routine-workspace`，需要时创建）。
fn workspace_dir() -> Result<String, String> {
    let dir = prism_dir()
        .ok_or_else(|| "无法定位 ~/.prism 数据目录".to_string())?
        .join(WORKSPACE_DIR);
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建例行工作区失败：{error}"))?;
    Ok(dir.to_string_lossy().into_owned())
}

/// 触发一次运行（后台线程）；同一任务已在跑则报错。
///
/// 状态用 `try_state` 取：`routines` 插件没装配时这里是「功能不可用」，
/// 而不是 `state()` 直接 panic。插件装配是可配置的，取状态就得是可失败的。
pub fn run_in_background(app: &AppHandle, routine: RoutineDefinition) -> Result<(), String> {
    let Some(state) = app.try_state::<RoutinesState>() else {
        return Err("定时任务插件未启用".to_string());
    };
    if !state.try_mark_running(&routine.id) {
        return Err("该任务正在运行中".to_string());
    }
    #[cfg(debug_assertions)]
    println!("[prism] 例行任务已触发：{}（{}）", routine.name, routine.id);
    let app = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let mut outcome = execute(&app, &routine);
        outcome.finished_at = now_ms();
        // 收尾也要容错：跑到一半插件被卸了，至少别让"运行中"标记永远挂着。
        if let Some(state) = app.try_state::<RoutinesState>() {
            state.unmark_running(&routine.id);
        }
        finalize(&app, &routine, &outcome);
    });
    Ok(())
}

struct RunOutcome {
    status: String,
    session_id: Option<String>,
    error: Option<String>,
    directory: String,
    started_at: i64,
    finished_at: i64,
}

fn execute(app: &AppHandle, routine: &RoutineDefinition) -> RunOutcome {
    let started_at = now_ms();
    let mut outcome = RunOutcome {
        status: "error".to_string(),
        session_id: None,
        error: None,
        directory: String::new(),
        started_at,
        finished_at: started_at,
    };
    let directory = match workspace_dir() {
        Ok(directory) => directory,
        Err(error) => {
            outcome.error = Some(error);
            return outcome;
        }
    };
    outcome.directory = directory.clone();
    // 引擎从注册表取，不再按名字 match：加一个引擎不用回来补分支，
    // 而且"能不能跑定时任务"直接由该引擎有没有实现 trait 的方法决定。
    let target = {
        let Some(registry) = app.try_state::<Mutex<EngineRegistry>>() else {
            outcome.error = Some("引擎插件未装配，定时任务无法开工".to_string());
            return outcome;
        };
        let guard = match registry.lock() {
            Ok(guard) => guard,
            Err(error) => {
                outcome.error = Some(format!("引擎注册表不可用：{error}"));
                return outcome;
            }
        };
        // 取出来的是一份 owned 句柄（Arc），锁可以立即释放。
        guard.get(&routine.engine)
    };
    let Some(engine) = target else {
        outcome.error = Some(format!(
            "该引擎（{}）的运行轴尚在开发，暂不能跑定时任务",
            routine.engine
        ));
        return outcome;
    };
    run_engine(app, routine, &directory, &mut outcome, engine.as_ref());
    outcome
}

/// 跑一轮：新建会话 → 发提示词 → 等终态。
///
/// 原来 `run_opencode` / `run_codex` 是两份几乎逐行相同的函数，唯一差别是
/// 各自 `app.state::<…Runtime>()` 的类型。trait 补齐 `subscribe` /
/// `send_prompt` / `interrupt` 之后，两份合并成一份。
fn run_engine(
    app: &AppHandle,
    routine: &RoutineDefinition,
    directory: &str,
    outcome: &mut RunOutcome,
    engine: &dyn Engine,
) {
    let events = engine.subscribe();
    let created = match engine.create_session(directory) {
        Ok(created) => created,
        Err(error) => {
            outcome.error = Some(format!("新建会话失败：{error}"));
            return;
        }
    };
    outcome.session_id = Some(created.session_id.clone());
    if let Err(error) = engine.send_prompt(&created.session_id, &routine.prompt) {
        outcome.error = Some(format!("发送提示词失败：{error}"));
        return;
    }
    let terminal = wait_terminal(
        app,
        routine,
        &created.session_id,
        directory,
        events,
        |session| engine.interrupt(session),
    );
    apply_terminal(terminal, outcome);
}

enum Terminal {
    Succeeded,
    Failed(Option<String>),
    Interrupted,
    Timeout,
}

/// 等本轮收口；期间把权限请求广播给前端（用户没开着该会话也能收到）。
fn wait_terminal(
    app: &AppHandle,
    routine: &RoutineDefinition,
    session_id: &str,
    directory: &str,
    events: mpsc::Receiver<Value>,
    interrupt: impl Fn(&str) -> Result<(), String>,
) -> Terminal {
    let deadline = Instant::now() + RUN_TIMEOUT;
    while Instant::now() < deadline {
        let event = match events.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => event,
            // 只是这半秒没事件，继续等。
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            // 事件源断开：这轮不会再有终态事件了（比如引擎没有事件流），
            // 直接收口，别空转到 15 分钟。
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if event.pointer("/data/sessionID").and_then(Value::as_str) != Some(session_id) {
            continue;
        }
        match event.get("type").and_then(Value::as_str) {
            Some("permission.asked") => {
                emit_event(
                    app,
                    RoutineEventPayload {
                        kind: "permission".to_string(),
                        routine_id: routine.id.clone(),
                        name: routine.name.clone(),
                        engine: routine.engine.clone(),
                        directory: directory.to_string(),
                        session_id: Some(session_id.to_string()),
                        started_at: now_ms(),
                        status: None,
                        error: None,
                        request_id: event
                            .pointer("/data/id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    },
                );
            }
            Some("session.execution.succeeded") => return Terminal::Succeeded,
            Some("session.execution.interrupted") => return Terminal::Interrupted,
            Some("session.execution.failed") => {
                return Terminal::Failed(error_text(event.pointer("/data/error")));
            }
            _ => {}
        }
    }
    if let Err(error) = interrupt(session_id) {
        eprintln!("[prism] 例行任务超时打断失败：{error}");
    }
    Terminal::Timeout
}

fn apply_terminal(terminal: Terminal, outcome: &mut RunOutcome) {
    match terminal {
        Terminal::Succeeded => outcome.status = "succeeded".to_string(),
        Terminal::Failed(error) => {
            outcome.status = "failed".to_string();
            outcome.error = error;
        }
        Terminal::Interrupted => outcome.status = "interrupted".to_string(),
        Terminal::Timeout => {
            outcome.status = "timeout".to_string();
            outcome.error = Some("等待收口超时（已尝试打断）".to_string());
        }
    }
}

/// 引擎给出的错误可能是字符串或对象（取 message），与前端归约口径一致。
fn error_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Object(map) => map
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// 收口：写日志 → 更新定义状态 → 广播事件。
fn finalize(app: &AppHandle, routine: &RoutineDefinition, outcome: &RunOutcome) {
    if let Some(dir) = store::logs_dir() {
        let entry = RoutineLogEntry {
            started_at: outcome.started_at,
            finished_at: Some(outcome.finished_at),
            status: outcome.status.clone(),
            session_id: outcome.session_id.clone(),
            engine: routine.engine.clone(),
            directory: outcome.directory.clone(),
            error: outcome.error.clone(),
        };
        if let Err(error) = store::write_log(&dir, &routine.id, &entry) {
            eprintln!("[prism] 写例行日志失败：{error}");
        }
    }

    if let Some(path) = store::store_path() {
        match store::load_all(&path) {
            Ok(mut routines) => {
                if let Some(target) = routines.iter_mut().find(|item| item.id == routine.id) {
                    target.last_trigger = Some(outcome.started_at);
                    target.last_status = Some(outcome.status.clone());
                    target.last_session_id = outcome.session_id.clone();
                }
                if let Err(error) = store::save_all(&path, &routines) {
                    eprintln!("[prism] 保存例行任务状态失败：{error}");
                }
            }
            Err(error) => eprintln!("[prism] 读取例行任务失败：{error}"),
        }
    }

    let kind = match outcome.status.as_str() {
        "succeeded" | "interrupted" => "done",
        _ => "failed",
    };
    emit_event(
        app,
        RoutineEventPayload {
            kind: kind.to_string(),
            routine_id: routine.id.clone(),
            name: routine.name.clone(),
            engine: routine.engine.clone(),
            directory: outcome.directory.clone(),
            session_id: outcome.session_id.clone(),
            started_at: outcome.started_at,
            status: Some(outcome.status.clone()),
            error: outcome.error.clone(),
            request_id: None,
        },
    );
}

fn emit_event(app: &AppHandle, payload: RoutineEventPayload) {
    let _ = app.emit("routine-event", payload);
}
