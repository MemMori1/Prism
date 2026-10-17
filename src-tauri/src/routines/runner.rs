//! 定时任务执行器：走引擎运行轴（新建会话 → 发提示词 → 等终态），
//! 记执行日志、更新定义状态，并广播 `routine-event` 供前端通知 / 刷新。
//!
//! 互斥：调度触发与"立即运行"共用 `RoutinesState.running` 集合（进程内）；
//! 进程退出即释放，无需文件锁（与 launchd 方案不同，见 ROADMAP M11 取舍）。
//! 权限：运行中若引擎请求审批，广播 permission 事件（用户点通知即可去处理）。

use serde::Serialize;
use serde_json::Value;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use super::scheduler::RoutinesState;
use super::store::{self, RoutineDefinition, RoutineLogEntry};
use crate::engines::codex::runtime::Runtime as CodexRuntime;
use crate::engines::opencode::runtime::Runtime as OpencodeRuntime;
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
pub fn run_in_background(app: &AppHandle, routine: RoutineDefinition) -> Result<(), String> {
    let state = app.state::<RoutinesState>();
    if !state.try_mark_running(&routine.id) {
        return Err("该任务正在运行中".to_string());
    }
    #[cfg(debug_assertions)]
    println!("[prism] 例行任务已触发：{}（{}）", routine.name, routine.id);
    let app = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let mut outcome = execute(&app, &routine);
        outcome.finished_at = now_ms();
        let state = app.state::<RoutinesState>();
        state.unmark_running(&routine.id);
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
    match routine.engine.as_str() {
        "opencode" => run_opencode(app, routine, &directory, &mut outcome),
        "codex" => run_codex(app, routine, &directory, &mut outcome),
        other => {
            outcome.error = Some(format!("该引擎（{other}）的运行轴尚在开发，暂不能跑定时任务"));
        }
    }
    outcome
}

fn run_opencode(app: &AppHandle, routine: &RoutineDefinition, directory: &str, outcome: &mut RunOutcome) {
    let runtime = app.state::<OpencodeRuntime>().inner().clone();
    let events = runtime.subscribe();
    let created = match runtime.create_session(directory) {
        Ok(created) => created,
        Err(error) => {
            outcome.error = Some(format!("新建会话失败：{error}"));
            return;
        }
    };
    outcome.session_id = Some(created.session_id.clone());
    if let Err(error) = runtime.send_prompt(&created.session_id, &routine.prompt) {
        outcome.error = Some(format!("发送提示词失败：{error}"));
        return;
    }
    let terminal = wait_terminal(app, routine, &created.session_id, directory, events, |session| {
        runtime.interrupt(session)
    });
    apply_terminal(terminal, outcome);
}

fn run_codex(app: &AppHandle, routine: &RoutineDefinition, directory: &str, outcome: &mut RunOutcome) {
    let runtime = app.state::<CodexRuntime>().inner().clone();
    let events = runtime.subscribe();
    let created = match runtime.create_session(directory) {
        Ok(created) => created,
        Err(error) => {
            outcome.error = Some(format!("新建会话失败：{error}"));
            return;
        }
    };
    outcome.session_id = Some(created.session_id.clone());
    if let Err(error) = runtime.send_prompt(&created.session_id, &routine.prompt) {
        outcome.error = Some(format!("发送提示词失败：{error}"));
        return;
    }
    let terminal = wait_terminal(app, routine, &created.session_id, directory, events, |session| {
        runtime.interrupt(session)
    });
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
        let Ok(event) = events.recv_timeout(Duration::from_millis(500)) else {
            continue;
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
