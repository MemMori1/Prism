//! Tauri IPC 命令：前端唯一入口。
//!
//! 约定：命令层只做参数整理与线程调度，业务逻辑一律下沉到各引擎适配器。

use std::path::{Path, PathBuf};

use crate::engines::claude;
use crate::engines::codex;
use crate::engines::codex::runtime::Runtime as CodexRuntime;
use crate::engines::core::{
    EngineHealth, ProjectItem, SearchHit, SearchStats, SessionSummary, TimelinePage, ToolDetail,
};
use crate::engines::opencode;
use crate::engines::opencode::runtime::{CreatedSession, PermissionRequest, Runtime, RuntimeStatus};
use crate::engines::quota;
use crate::engines::usage::{self, UsageSummary};
use crate::engines::{mcp, workshop};
use crate::routines::{runner, scheduler::RoutinesState, store};
use crate::routines::store::{RoutineDefinition, RoutineLogEntry, RoutineView};

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

/// 已知项目工作目录：三引擎项目列表合并去重（工坊扫描项目级资产用）。
pub fn known_project_cwds() -> Vec<String> {
    let mut cwds: Vec<String> = Vec::new();
    if let Some(db) = opencode::locator::database_path() {
        if let Ok(list) = opencode::db::list_projects(&db) {
            cwds.extend(list.into_iter().map(|item| item.worktree));
        }
    }
    if let Some(db) = codex::locator::state_db() {
        if let Ok(list) = codex::state::list_projects(&db) {
            cwds.extend(list.into_iter().map(|item| item.worktree));
        }
    }
    if let Some(root) = claude::locator::projects_dir() {
        if let Ok(list) = claude::transcript::list_projects(&root) {
            cwds.extend(list.into_iter().map(|item| item.worktree));
        }
    }
    cwds.sort();
    cwds.dedup();
    cwds.retain(|cwd| Path::new(cwd).is_dir());
    cwds
}

/// 会话类型判定：Codex 会话（`codex:` 前缀）走 codex 运行轴。
fn is_codex_session(session_id: &str) -> bool {
    codex::state::is_codex_session(session_id)
}

/// 全部引擎的健康状态：历史（数据文件）与运行时（CLI）分别探测。
#[tauri::command]
pub async fn engine_health() -> Result<Vec<EngineHealth>, String> {
    let opencode_db = opencode::locator::database_path();
    let opencode_cli = opencode::locator::cli_path();
    let codex_db = codex::locator::state_db();
    let codex_cli = codex::locator::cli_path();
    // Claude 的历史数据是目录（存在才算就绪）；CLI 决定运行轴（后置）。
    let claude_root = claude::locator::projects_dir().filter(|path| path.is_dir());
    let claude_cli = claude::locator::cli_path();
    #[cfg(debug_assertions)]
    println!(
        "[prism] engine_health: opencode db={:?} cli={:?} / codex db={:?} cli={:?} / claude dir={:?} cli={:?}",
        opencode_db, opencode_cli, codex_db, codex_cli, claude_root, claude_cli
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
        EngineHealth {
            engine_id: claude::locator::ENGINE_ID.to_string(),
            display_name: claude::locator::DISPLAY_NAME.to_string(),
            // 浏览历史只要求档案目录存在；CLI 决定运行轴（后置里程碑）。
            ready: claude_root.is_some(),
            database_path: claude_root.map(|path| path.display().to_string()),
            cli_path: claude_cli.map(|path| path.display().to_string()),
        },
    ])
}

/// 项目列表：各引擎合并（任一引擎缺数据不拖垮另一个），按最近活动排序。
#[tauri::command]
pub async fn list_projects() -> Result<Vec<ProjectItem>, String> {
    let opencode_db = opencode::locator::database_path();
    let codex_db = codex::locator::state_db();
    let claude_root = claude::locator::projects_dir();
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
        if let Some(root) = claude_root {
            match claude::transcript::list_projects(&root) {
                Ok(mut list) => items.append(&mut list),
                Err(cause) => eprintln!("[prism] list_projects(claude) 失败：{cause}"),
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

    if claude::transcript::is_claude_project(&project_id) {
        let root = claude::locator::projects_dir()
            .ok_or_else(|| "无法定位 Claude 档案目录".to_string())?;
        let dir_name = claude::transcript::dir_name_of(&project_id)
            .ok_or_else(|| "非法的 Claude 项目 id".to_string())?
            .to_string();
        let limit = limit.unwrap_or(500).clamp(1, 1000);
        return tauri::async_runtime::spawn_blocking(move || {
            claude::transcript::list_sessions(&root, &dir_name, limit)
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
    if claude::transcript::is_claude_session(&session_id) {
        let root = claude::locator::projects_dir()
            .ok_or_else(|| "无法定位 Claude 档案目录".to_string())?;
        let uuid = claude::transcript::session_uuid_of(&session_id)
            .unwrap_or_default()
            .to_string();
        return tauri::async_runtime::spawn_blocking(move || {
            claude::transcript::session_summary(&root, &uuid)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }
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

    if claude::transcript::is_claude_session(&session_id) {
        let root = claude::locator::projects_dir()
            .ok_or_else(|| "无法定位 Claude 档案目录".to_string())?;
        let uuid = claude::transcript::session_uuid_of(&session_id)
            .unwrap_or_default()
            .to_string();
        let page = tauri::async_runtime::spawn_blocking(move || {
            let Some((_, path)) = claude::transcript::find_session_file(&root, &uuid) else {
                return Err("Claude 会话档案不存在".to_string());
            };
            claude::transcript::read_timeline(&path, before_seq, limit)
        })
        .await
        .map_err(|cause| cause.to_string())?;
        let page = page?;
        #[cfg(debug_assertions)]
        println!(
            "[prism] read_timeline(claude) -> {} 条 (has_more={})",
            page.records.len(),
            page.has_more
        );
        return Ok(page);
    }

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
    if claude::transcript::is_claude_session(&session_id) {
        let root = claude::locator::projects_dir()
            .ok_or_else(|| "无法定位 Claude 档案目录".to_string())?;
        let uuid = claude::transcript::session_uuid_of(&session_id)
            .unwrap_or_default()
            .to_string();
        return tauri::async_runtime::spawn_blocking(move || {
            let Some((_, path)) = claude::transcript::find_session_file(&root, &uuid) else {
                return Err("Claude 会话档案不存在".to_string());
            };
            claude::transcript::read_tool_detail(&path, &message_id, &call_id)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }

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

/// 在指定目录新建会话（首次调用会拉起对应引擎的运行轴，可能稍慢）。
#[tauri::command]
pub async fn session_create(
    runtime: tauri::State<'_, Runtime>,
    codex_runtime: tauri::State<'_, CodexRuntime>,
    directory: String,
    engine: Option<String>,
) -> Result<CreatedSession, String> {
    #[cfg(debug_assertions)]
    println!("[prism] session_create -> {directory}（engine={:?}）", engine);
    match engine.as_deref() {
        None | Some("opencode") => {
            let runtime = runtime.inner().clone();
            tauri::async_runtime::spawn_blocking(move || runtime.create_session(&directory))
                .await
                .map_err(|cause| cause.to_string())?
        }
        Some("codex") => {
            let codex_runtime = codex_runtime.inner().clone();
            tauri::async_runtime::spawn_blocking(move || codex_runtime.create_session(&directory))
                .await
                .map_err(|cause| cause.to_string())?
        }
        Some(other) => Err(format!("该引擎（{other}）的运行轴尚在开发，暂不能新建会话")),
    }
}

/// 发送一条用户消息，返回用户消息 id（Codex 无此 id，返回空串）。
#[tauri::command]
pub async fn session_send(
    runtime: tauri::State<'_, Runtime>,
    codex_runtime: tauri::State<'_, CodexRuntime>,
    session_id: String,
    text: String,
) -> Result<String, String> {
    #[cfg(debug_assertions)]
    println!(
        "[prism] session_send -> {}（{} 字符）",
        session_id,
        text.chars().count()
    );
    if is_codex_session(&session_id) {
        let codex_runtime = codex_runtime.inner().clone();
        return tauri::async_runtime::spawn_blocking(move || {
            codex_runtime.send_prompt(&session_id, &text)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.send_prompt(&session_id, &text))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 中断当前执行。
#[tauri::command]
pub async fn session_interrupt(
    runtime: tauri::State<'_, Runtime>,
    codex_runtime: tauri::State<'_, CodexRuntime>,
    session_id: String,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    println!("[prism] session_interrupt -> {session_id}");
    if is_codex_session(&session_id) {
        let codex_runtime = codex_runtime.inner().clone();
        return tauri::async_runtime::spawn_blocking(move || codex_runtime.interrupt(&session_id))
            .await
            .map_err(|cause| cause.to_string())?;
    }
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.interrupt(&session_id))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 会话当前挂起的权限请求（选择会话时补拉；运行轴没开则返回空）。
#[tauri::command]
pub async fn session_permissions(
    runtime: tauri::State<'_, Runtime>,
    codex_runtime: tauri::State<'_, CodexRuntime>,
    session_id: String,
) -> Result<Vec<PermissionRequest>, String> {
    if is_codex_session(&session_id) {
        let codex_runtime = codex_runtime.inner().clone();
        return tauri::async_runtime::spawn_blocking(move || {
            codex_runtime.list_permissions(&session_id)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.list_permissions(&session_id))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 回复权限请求：once / always / reject。
#[tauri::command]
pub async fn session_permission_reply(
    runtime: tauri::State<'_, Runtime>,
    codex_runtime: tauri::State<'_, CodexRuntime>,
    session_id: String,
    request_id: String,
    decision: String,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    println!("[prism] session_permission_reply -> {session_id} {request_id} {decision}");
    if is_codex_session(&session_id) {
        let codex_runtime = codex_runtime.inner().clone();
        return tauri::async_runtime::spawn_blocking(move || {
            codex_runtime.permission_reply(&session_id, &request_id, &decision)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }
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

// ---------------------------------------------------------------- 用量

/// 用量统计：跨引擎 token 汇总与成本估算（days = 窗口天数，0 = 全部）。
#[tauri::command]
pub async fn usage_summary(days: u32) -> Result<UsageSummary, String> {
    let summary = tauri::async_runtime::spawn_blocking(move || usage::summary(days))
        .await
        .map_err(|cause| cause.to_string())??;
    #[cfg(debug_assertions)]
    println!(
        "[prism] usage_summary -> days={} 会话={} 成本≈${:.4}",
        summary.days, summary.sessions, summary.cost_usd
    );
    Ok(summary)
}

/// 配额实时值：Codex 订阅窗口（一次性 app-server 读取，5 分钟缓存；force=true 强制刷新）。
#[tauri::command]
pub async fn quota_summary(
    force: bool,
    state: tauri::State<'_, quota::QuotaState>,
) -> Result<quota::QuotaSummary, String> {
    if !force {
        if let Some(cached) = state.fresh() {
            return Ok(cached);
        }
    }
    let fetched = tauri::async_runtime::spawn_blocking(quota::collect)
        .await
        .map_err(|cause| cause.to_string())?;
    state.store(fetched.clone());
    #[cfg(debug_assertions)]
    println!(
        "[prism] quota_summary -> force={} codex={} claude={}",
        force, fetched.codex.state, fetched.claude.state
    );
    Ok(fetched)
}

/// 工坊：三引擎资产总览（技能 / 命令 / 子代理 / 记忆 / 插件，只读）。
#[tauri::command]
pub async fn workshop_assets() -> Result<Vec<workshop::AssetItem>, String> {
    let items = tauri::async_runtime::spawn_blocking(|| {
        let roots = workshop::AssetRoots {
            opencode_config: opencode::locator::config_dir(),
            codex_home: codex::locator::codex_home(),
            claude_dir: claude::locator::config_root(),
        };
        workshop::collect(&roots, &known_project_cwds())
    })
    .await
    .map_err(|cause| cause.to_string())?;
    #[cfg(debug_assertions)]
    println!("[prism] workshop_assets -> {} 条", items.len());
    Ok(items)
}

/// 工坊：读取资产入口文件（预览，超限截断并标记）。
#[tauri::command]
pub async fn workshop_read(path: String) -> Result<workshop::AssetFile, String> {
    let target = path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (content, truncated) = workshop::read_text(Path::new(&target), 200_000)?;
        Ok(workshop::AssetFile {
            path: target,
            content,
            truncated,
        })
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 用系统默认程序打开路径（只读操作，不写引擎目录）。
#[tauri::command]
pub async fn open_path(path: String) -> Result<(), String> {
    let target = PathBuf::from(&path);
    if !target.exists() {
        return Err(format!("路径不存在：{path}"));
    }
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    std::process::Command::new(opener)
        .arg(&target)
        .spawn()
        .map_err(|cause| format!("打开失败：{cause}"))?;
    Ok(())
}

/// 工坊：MCP 服务清单（三引擎配置真相位置，只读解析；环境变量只给键名）。
#[tauri::command]
pub async fn workshop_mcp() -> Result<Vec<mcp::McpServer>, String> {
    let servers = tauri::async_runtime::spawn_blocking(|| {
        let roots = mcp::McpRoots {
            home: std::env::var_os("HOME").map(PathBuf::from),
            opencode_config: opencode::locator::config_dir(),
            codex_home: codex::locator::codex_home(),
        };
        mcp::collect(&roots, &known_project_cwds())
    })
    .await
    .map_err(|cause| cause.to_string())?;
    #[cfg(debug_assertions)]
    println!("[prism] workshop_mcp -> {} 个服务", servers.len());
    Ok(servers)
}

/// MCP 探活：只对 http 发 GET（任何 HTTP 响应=可达），绝不 spawn stdio。
#[tauri::command]
pub async fn mcp_probe(url: String) -> Result<mcp::ProbeResult, String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("探活仅支持 http(s) 地址".to_string());
    }
    let result = tauri::async_runtime::spawn_blocking(move || {
        mcp::probe_http(&url, std::time::Duration::from_secs(4))
    })
    .await
    .map_err(|cause| cause.to_string())?;
    Ok(result)
}

// ---------------------------------------------------------------- 定时任务（M11b）

fn routines_store_path() -> Result<PathBuf, String> {
    store::store_path().ok_or_else(|| "无法定位 ~/.prism 数据目录".to_string())
}

/// 任务列表（含派生字段：下次触发、是否运行中）。
#[tauri::command]
pub async fn routines_list(state: tauri::State<'_, RoutinesState>) -> Result<Vec<RoutineView>, String> {
    let definitions = store::load_all(&routines_store_path()?)?;
    let now = runner::now_ms();
    Ok(definitions
        .iter()
        .map(|item| store::to_view(item, now, state.is_running(&item.id)))
        .collect())
}

/// 新建任务（调度表达式立即校验；触发时刻由调度器 30s 轮询接管）。
#[tauri::command]
pub async fn routine_create(
    name: String,
    schedule: String,
    prompt: String,
    engine: String,
) -> Result<RoutineView, String> {
    store::validate(&name, &schedule, &prompt, &engine)?;
    let path = routines_store_path()?;
    let mut definitions = store::load_all(&path)?;
    let now = runner::now_ms();
    let definition = RoutineDefinition {
        id: store::new_id(now),
        name: name.trim().to_string(),
        schedule: schedule.trim().to_string(),
        prompt: prompt.trim().to_string(),
        engine,
        enabled: true,
        created_at: now,
        last_trigger: None,
        last_status: None,
        last_session_id: None,
    };
    definitions.push(definition.clone());
    store::save_all(&path, &definitions)?;
    #[cfg(debug_assertions)]
    println!("[prism] routine_create -> {}（{}）", definition.name, definition.id);
    Ok(store::to_view(&definition, now, false))
}

/// 编辑任务；只传需要改的字段（合并后整体校验）。
#[tauri::command]
pub async fn routine_update(
    id: String,
    name: Option<String>,
    schedule: Option<String>,
    prompt: Option<String>,
    engine: Option<String>,
    enabled: Option<bool>,
) -> Result<RoutineView, String> {
    let path = routines_store_path()?;
    let mut definitions = store::load_all(&path)?;
    let definition = definitions
        .iter_mut()
        .find(|item| item.id == id)
        .ok_or_else(|| "任务不存在（可能已被删除）".to_string())?;
    if let Some(value) = name {
        definition.name = value.trim().to_string();
    }
    if let Some(value) = schedule {
        definition.schedule = value.trim().to_string();
    }
    if let Some(value) = prompt {
        definition.prompt = value.trim().to_string();
    }
    if let Some(value) = engine {
        definition.engine = value;
    }
    if let Some(value) = enabled {
        definition.enabled = value;
    }
    store::validate(
        &definition.name,
        &definition.schedule,
        &definition.prompt,
        &definition.engine,
    )?;
    let snapshot = definition.clone();
    store::save_all(&path, &definitions)?;
    Ok(store::to_view(&snapshot, runner::now_ms(), false))
}

#[tauri::command]
pub async fn routine_delete(id: String) -> Result<(), String> {
    let path = routines_store_path()?;
    let mut definitions = store::load_all(&path)?;
    let before = definitions.len();
    definitions.retain(|item| item.id != id);
    if definitions.len() == before {
        return Err("任务不存在（可能已被删除）".to_string());
    }
    store::save_all(&path, &definitions)
}

/// 立即运行（不看 enabled；与前台的"立即运行"按钮对应）。
#[tauri::command]
pub async fn routine_run(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let definitions = store::load_all(&routines_store_path()?)?;
    let definition = definitions
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| "任务不存在（可能已被删除）".to_string())?;
    runner::run_in_background(&app, definition)
}

/// 任务执行日志（新到旧）。
#[tauri::command]
pub async fn routine_logs(id: String, limit: Option<usize>) -> Result<Vec<RoutineLogEntry>, String> {
    let dir = store::logs_dir().ok_or_else(|| "无法定位 ~/.prism 数据目录".to_string())?;
    Ok(store::read_logs(&dir, &id, limit.unwrap_or(20)))
}

/// 托盘状态刷新：前端把「执行中 / 待审批」实时推给菜单栏（尽力而为，失败静默）。
#[tauri::command]
pub fn tray_status(app: tauri::AppHandle, running: u32, waiting: u32) {
    crate::tray::update_status(&app, running, waiting);
}
