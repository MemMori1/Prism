//! Tauri IPC 命令：前端唯一入口。
//!
//! 约定：命令层只做参数整理与线程调度，业务逻辑一律下沉到各引擎适配器。

use std::path::PathBuf;

use crate::engines::core::{
    EngineHealth, ProjectItem, SessionSummary, TimelinePage, ToolDetail,
};
use crate::engines::opencode;

fn opencode_db_path() -> Result<PathBuf, String> {
    opencode::locator::database_path().ok_or_else(|| {
        "未找到 OpenCode 数据库（~/.local/share/opencode/opencode.db），请先用 OpenCode 跑过会话。"
            .to_string()
    })
}

/// OpenCode 引擎健康状态：历史（数据库）与运行时（CLI）分别探测。
#[tauri::command]
pub async fn engine_health() -> Result<EngineHealth, String> {
    let database_path = opencode::locator::database_path();
    let cli_path = opencode::locator::cli_path();
    #[cfg(debug_assertions)]
    println!(
        "[prism] engine_health: db={:?} cli={:?}",
        database_path, cli_path
    );
    Ok(EngineHealth {
        engine_id: opencode::locator::ENGINE_ID.to_string(),
        display_name: opencode::locator::DISPLAY_NAME.to_string(),
        ready: database_path.is_some() && cli_path.is_some(),
        database_path: database_path.map(|path| path.display().to_string()),
        cli_path: cli_path.map(|path| path.display().to_string()),
    })
}

#[tauri::command]
pub async fn list_projects() -> Result<Vec<ProjectItem>, String> {
    let db_path = opencode_db_path()?;
    let items = tauri::async_runtime::spawn_blocking(move || opencode::db::list_projects(&db_path))
        .await
        .map_err(|cause| cause.to_string())?;
    let items = items?;
    #[cfg(debug_assertions)]
    println!("[prism] list_projects -> {} 项", items.len());
    Ok(items)
}

#[tauri::command]
pub async fn list_sessions(
    project_id: String,
    limit: Option<i64>,
) -> Result<Vec<SessionSummary>, String> {
    let db_path = opencode_db_path()?;
    let limit = limit
        .unwrap_or_else(opencode::db::session_limit)
        .clamp(1, opencode::db::session_limit());
    tauri::async_runtime::spawn_blocking(move || {
        opencode::db::list_sessions(&db_path, &project_id, limit)
    })
    .await
    .map_err(|cause| cause.to_string())?
}

#[tauri::command]
pub async fn read_timeline(
    session_id: String,
    before_seq: Option<i64>,
    limit: Option<i64>,
) -> Result<TimelinePage, String> {
    let db_path = opencode_db_path()?;
    let limit = limit
        .unwrap_or_else(opencode::db::timeline_page_size)
        .clamp(1, 500);
    let page = tauri::async_runtime::spawn_blocking(move || {
        opencode::db::read_timeline(&db_path, &session_id, before_seq, limit)
    })
    .await
    .map_err(|cause| cause.to_string())?;
    let page = page?;
    #[cfg(debug_assertions)]
    println!(
        "[prism] read_timeline -> {} 条 (has_more={})",
        page.records.len(),
        page.has_more
    );
    Ok(page)
}

/// 展开工具卡片时按需加载完整输入 / 输出 / diff。
#[tauri::command]
pub async fn read_tool_detail(
    session_id: String,
    message_id: String,
    call_id: String,
) -> Result<ToolDetail, String> {
    let db_path = opencode_db_path()?;
    let detail = tauri::async_runtime::spawn_blocking(move || {
        opencode::db::read_tool_detail(&db_path, &session_id, &message_id, &call_id)
    })
    .await
    .map_err(|cause| cause.to_string())?;
    let detail = detail?;
    #[cfg(debug_assertions)]
    println!(
        "[prism] read_tool_detail -> ok（output={}，diff={}）",
        detail.output.is_some(),
        detail
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("files"))
            .is_some()
    );
    Ok(detail)
}
