//! Tauri IPC 命令：前端唯一入口。
//!
//! 约定：命令层只做参数整理与线程调度，业务逻辑一律下沉到各引擎适配器。

use std::path::{Path, PathBuf};

use crate::engines::codex;
use crate::engines::core::{
    EngineHealth, ProjectItem, SearchHit, SearchStats, SessionSummary, TimelinePage, ToolDetail,
};
use crate::engines::opencode;
use crate::engines::opencode::runtime::{CreatedSession, PermissionRequest, Runtime, RuntimeStatus};

fn opencode_db_path() -> Result<PathBuf, String> {
    opencode::locator::database_path().ok_or_else(|| {
        "未找到 OpenCode 数据库（~/.local/share/opencode/opencode.db），请先用 OpenCode 跑过会话。"
            .to_string()
    })
}

fn codex_state_db_path() -> Result<PathBuf, String> {
    codex::locator::state_db().ok_or_else(|| {
        "未找到 Codex 状态库（~/.codex/state_*.sqlite），请先安装 Codex 并跑过会话。".to_string()
    })
}

/// Codex 运行轴尚未接入：这些命令对 Codex 会话一律拒绝（浏览不受影响）。
fn reject_codex_runtime(session_id: &str) -> Result<(), String> {
    if codex::state::is_codex_session(session_id) {
        return Err("Codex 会话暂为只读浏览（运行轴开发中），请到 Codex.app 继续对话。".to_string());
    }
    Ok(())
}

/// 全部引擎的健康状态：历史（数据文件）与运行时（CLI）分别探测。
#[tauri::command]
pub async fn engine_health() -> Result<Vec<EngineHealth>, String> {
    let opencode_db = opencode::locator::database_path();
    let opencode_cli = opencode::locator::cli_path();
    let codex_db = codex::locator::state_db();
    let codex_cli = codex::locator::cli_path();
    #[cfg(debug_assertions)]
    println!(
        "[prism] engine_health: opencode db={:?} cli={:?} / codex db={:?} cli={:?}",
        opencode_db, opencode_cli, codex_db, codex_cli
    );
    Ok(vec![
        EngineHealth {
            engine_id: opencode::locator::ENGINE_ID.to_string(),
            display_name: opencode::locator::DISPLAY_NAME.to_string(),
            ready: opencode_db.is_some() && opencode_cli.is_some(),
            database_path: opencode_db.map(|path| path.display().to_string()),
            cli_path: opencode_cli.map(|path| path.display().to_string()),
        },
        EngineHealth {
            engine_id: codex::locator::ENGINE_ID.to_string(),
            display_name: codex::locator::DISPLAY_NAME.to_string(),
            // 浏览历史只要求状态库存在；CLI 决定能否运行（运行轴开发中）。
            ready: codex_db.is_some(),
            database_path: codex_db.map(|path| path.display().to_string()),
            cli_path: codex_cli.map(|path| path.display().to_string()),
        },
    ])
}

/// 项目列表：各引擎合并（任一引擎缺数据不拖垮另一个），按最近活动排序。
#[tauri::command]
pub async fn list_projects() -> Result<Vec<ProjectItem>, String> {
    let opencode_db = opencode::locator::database_path();
    let codex_db = codex::locator::state_db();
    let items = tauri::async_runtime::spawn_blocking(move || {
        let mut items: Vec<ProjectItem> = Vec::new();
        if let Some(db_path) = opencode_db {
            match opencode::db::list_projects(&db_path) {
                Ok(mut list) => items.append(&mut list),
                Err(cause) => eprintln!("[prism] list_projects(opencode) 失败：{cause}"),
            }
        }
        if let Some(db_path) = codex_db {
            match codex::state::list_projects(&db_path) {
                Ok(mut list) => items.append(&mut list),
                Err(cause) => eprintln!("[prism] list_projects(codex) 失败：{cause}"),
            }
        }
        items.sort_by(|a, b| b.time_updated.cmp(&a.time_updated));
        items
    })
    .await
    .map_err(|cause| cause.to_string())?;
    #[cfg(debug_assertions)]
    println!("[prism] list_projects -> {} 项", items.len());
    Ok(items)
}

#[tauri::command]
pub async fn list_sessions(
    project_id: String,
    limit: Option<i64>,
) -> Result<Vec<SessionSummary>, String> {
    if codex::state::is_codex_project(&project_id) {
        let db_path = codex_state_db_path()?;
        let cwd = codex::state::cwd_of(&project_id)
            .ok_or_else(|| "非法的 Codex 项目 id".to_string())?
            .to_string();
        let limit = limit.unwrap_or(500).clamp(1, 1000);
        return tauri::async_runtime::spawn_blocking(move || {
            codex::state::list_sessions(&db_path, &cwd, limit)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }

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

/// 按 id 读取单个会话概要（工作台恢复 / 刷新列时用）。
#[tauri::command]
pub async fn session_summary(session_id: String) -> Result<Option<SessionSummary>, String> {
    if codex::state::is_codex_session(&session_id) {
        let db_path = codex_state_db_path()?;
        let thread_id = codex::state::thread_id_of(&session_id)
            .unwrap_or_default()
            .to_string();
        return tauri::async_runtime::spawn_blocking(move || {
            codex::state::get_session(&db_path, &thread_id)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }

    let db_path = opencode_db_path()?;
    tauri::async_runtime::spawn_blocking(move || {
        opencode::db::get_session(&db_path, &session_id)
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
    let limit = limit
        .unwrap_or_else(opencode::db::timeline_page_size)
        .clamp(1, 500);

    if codex::state::is_codex_session(&session_id) {
        let db_path = codex_state_db_path()?;
        let thread_id = codex::state::thread_id_of(&session_id)
            .unwrap_or_default()
            .to_string();
        let page = tauri::async_runtime::spawn_blocking(move || {
            let rollout_path = codex::state::rollout_path_of(&db_path, &thread_id)?
                .ok_or_else(|| "Codex 会话没有 rollout 文件".to_string())?;
            codex::rollout::read_timeline(Path::new(&rollout_path), before_seq, limit)
        })
        .await
        .map_err(|cause| cause.to_string())?;
        let page = page?;
        #[cfg(debug_assertions)]
        println!(
            "[prism] read_timeline(codex) -> {} 条 (has_more={})",
            page.records.len(),
            page.has_more
        );
        return Ok(page);
    }

    let db_path = opencode_db_path()?;
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
    if codex::state::is_codex_session(&session_id) {
        let db_path = codex_state_db_path()?;
        let thread_id = codex::state::thread_id_of(&session_id)
            .unwrap_or_default()
            .to_string();
        return tauri::async_runtime::spawn_blocking(move || {
            let rollout_path = codex::state::rollout_path_of(&db_path, &thread_id)?
                .ok_or_else(|| "Codex 会话没有 rollout 文件".to_string())?;
            codex::rollout::read_tool_detail(Path::new(&rollout_path), &message_id, &call_id)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }

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

/// OpenCode CLI 版本（`--version` 首行；未安装或读取失败返回 null）。
#[tauri::command]
pub async fn cli_version() -> Result<Option<String>, String> {
    let cli = opencode::locator::cli_path();
    let version = tauri::async_runtime::spawn_blocking(move || {
        cli.and_then(|path| opencode::locator::cli_version(&path))
    })
    .await
    .map_err(|cause| cause.to_string())?;
    Ok(version)
}

// ---------------------------------------------------------------- 运行轴

/// 运行轴状态（不触发 serve 启动）。
#[tauri::command]
pub async fn runtime_status(runtime: tauri::State<'_, Runtime>) -> Result<RuntimeStatus, String> {
    Ok(runtime.status())
}

/// 在指定目录新建会话（首次调用会启动 serve，可能稍慢）。
#[tauri::command]
pub async fn session_create(
    runtime: tauri::State<'_, Runtime>,
    directory: String,
) -> Result<CreatedSession, String> {
    #[cfg(debug_assertions)]
    println!("[prism] session_create -> {directory}");
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.create_session(&directory))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 发送一条用户消息，返回用户消息 id。
#[tauri::command]
pub async fn session_send(
    runtime: tauri::State<'_, Runtime>,
    session_id: String,
    text: String,
) -> Result<String, String> {
    reject_codex_runtime(&session_id)?;
    #[cfg(debug_assertions)]
    println!(
        "[prism] session_send -> {}（{} 字符）",
        session_id,
        text.chars().count()
    );
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.send_prompt(&session_id, &text))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 中断当前执行。
#[tauri::command]
pub async fn session_interrupt(
    runtime: tauri::State<'_, Runtime>,
    session_id: String,
) -> Result<(), String> {
    reject_codex_runtime(&session_id)?;
    #[cfg(debug_assertions)]
    println!("[prism] session_interrupt -> {session_id}");
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.interrupt(&session_id))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 会话当前挂起的权限请求（选择会话时补拉；serve 未运行则返回空）。
#[tauri::command]
pub async fn session_permissions(
    runtime: tauri::State<'_, Runtime>,
    session_id: String,
) -> Result<Vec<PermissionRequest>, String> {
    reject_codex_runtime(&session_id)?;
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.list_permissions(&session_id))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 回复权限请求：once / always / reject。
#[tauri::command]
pub async fn session_permission_reply(
    runtime: tauri::State<'_, Runtime>,
    session_id: String,
    request_id: String,
    decision: String,
) -> Result<(), String> {
    reject_codex_runtime(&session_id)?;
    #[cfg(debug_assertions)]
    println!("[prism] session_permission_reply -> {session_id} {request_id} {decision}");
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        runtime.permission_reply(&session_id, &request_id, &decision)
    })
    .await
    .map_err(|cause| cause.to_string())?
}

// ---------------------------------------------------------------- 搜索

/// 搜索索引状态（只读索引库，不触碰引擎数据）。
#[tauri::command]
pub async fn search_stats() -> Result<SearchStats, String> {
    let search_db = opencode::search::prism_search_db_path()
        .ok_or_else(|| "无法定位 ~/.prism 数据目录".to_string())?;
    tauri::async_runtime::spawn_blocking(move || opencode::search::stats(&search_db))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 增量构建 / 更新全文索引（Prism 自有数据，引擎库只读）。
#[tauri::command]
pub async fn search_update_index() -> Result<SearchStats, String> {
    let engine_db = opencode_db_path()?;
    let search_db = opencode::search::prism_search_db_path()
        .ok_or_else(|| "无法定位 ~/.prism 数据目录".to_string())?;
    let stats = tauri::async_runtime::spawn_blocking(move || {
        opencode::search::update_index(&engine_db, &search_db)
    })
    .await
    .map_err(|cause| cause.to_string())?;
    let stats = stats?;
    #[cfg(debug_assertions)]
    println!(
        "[prism] search_update_index -> {} 会话 / {} 消息",
        stats.indexed_sessions, stats.indexed_messages
    );
    Ok(stats)
}

/// 全文搜索：关键词 / 项目 / 时间下限 / 仅标题。
#[tauri::command]
pub async fn search_query(
    term: String,
    project_id: Option<String>,
    since: Option<i64>,
    only_title: Option<bool>,
    limit: Option<i64>,
) -> Result<Vec<SearchHit>, String> {
    let engine_db = opencode_db_path()?;
    let search_db = opencode::search::prism_search_db_path()
        .ok_or_else(|| "无法定位 ~/.prism 数据目录".to_string())?;
    let hits = tauri::async_runtime::spawn_blocking(move || {
        opencode::search::search(
            &engine_db,
            &search_db,
            &term,
            project_id.as_deref(),
            since,
            only_title.unwrap_or(false),
            limit,
        )
    })
    .await
    .map_err(|cause| cause.to_string())??;
    #[cfg(debug_assertions)]
    println!("[prism] search_query -> {} 条", hits.len());
    Ok(hits)
}
