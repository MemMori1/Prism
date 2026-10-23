//! Tauri IPC 命令：前端唯一入口。
//!
//! 约定：命令层只做参数整理与线程调度，业务逻辑一律下沉到各引擎适配器。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::agents::{self, AgentProfile};
use crate::dag::{self, DagPlan, DagSummary, Issue};
use crate::kernel::{self, KernelState, PluginInfo};
use crate::annotations::{self, AnnotationStore};
use crate::catalog::{self, EngineCatalog};
use crate::engines::claude;
use crate::engines::codex;
use crate::engines::codex::runtime::Runtime as CodexRuntime;
use crate::engines::core::{
    EngineHealth, ProjectItem, SearchHit, SearchStats, SessionSummary, TimelinePage, ToolDetail,
};
use crate::engines::dsh;
use crate::engines::dsh::runtime::Runtime as DshRuntime;
use crate::engines::native;
use crate::engines::opencode;
use crate::engines::opencode::runtime::{
    CreatedSession, PermissionRequest, Runtime, RuntimeStatus,
};
use crate::engines::quota;
use crate::engines::usage::{self, UsageSummary};
use crate::engines::{mcp, workshop};
use crate::files::{self, FileHit, GitBranches, GitSnapshot};
use crate::routines::store::{RoutineDefinition, RoutineLogEntry, RoutineView};
use crate::routines::{runner, scheduler::RoutinesState, store};
use crate::tasks::{SpawnRequest, TaskInfo, TasksState};

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
    cwds.extend(
        native::store::list_projects()
            .into_iter()
            .map(|item| item.worktree),
    );
    cwds.extend(
        dsh::history::list_projects(200)
            .into_iter()
            .map(|item| item.worktree),
    );
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
///
/// 数据源是引擎注册表——加一个引擎只需在 `lib.rs` 注册一行，
/// 这里不用再补一段 `EngineHealth { ... }`。
#[tauri::command]
pub async fn engine_health(
    registry: tauri::State<'_, crate::engines::engine::RegistryState>,
) -> Result<Vec<EngineHealth>, String> {
    let health = registry.lock().map_err(|cause| cause.to_string())?.health();
    #[cfg(debug_assertions)]
    for item in &health {
        println!(
            "[prism] engine_health: {} ready={} db={:?} cli={:?}",
            item.engine_id, item.ready, item.database_path, item.cli_path
        );
    }
    Ok(health)
}

// ---------------------------------------------------------------- dsh 权限

/// dsh 的权限档位视图：`mode` 为 `null` 表示没设过，用 dsh 自己的默认。
///
/// 返回可选值的同时把每个档位的说明也带上——设置页要显示"这个档位意味着什么"，
/// 而档位与 risk 的对应关系属于引擎知识，不该由前端再抄一份。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshPermissionView {
    pub mode: Option<String>,
    pub options: Vec<DshPermissionOption>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshPermissionOption {
    pub id: String,
    pub label: String,
    pub description: String,
    /// 全盘可写 + 不问，前端要额外提示。
    pub risky: bool,
}

fn dsh_permission_view() -> DshPermissionView {
    let options = [
        (dsh::permission::Mode::ReadOnly, "仅可查看", false),
        (dsh::permission::Mode::WorkspaceWrite, "工作区内修改", false),
        (dsh::permission::Mode::FullAccess, "完全权限", true),
    ]
    .into_iter()
    .map(|(mode, label, risky)| DshPermissionOption {
        id: mode.as_str().to_string(),
        label: label.to_string(),
        description: mode.description().to_string(),
        risky,
    })
    .collect();
    DshPermissionView {
        mode: dsh::permission::load().map(|mode| mode.as_str().to_string()),
        options,
    }
}

#[tauri::command]
pub async fn dsh_permission() -> Result<DshPermissionView, String> {
    Ok(dsh_permission_view())
}

/// 改 dsh 的权限档位；`mode` 传 `null` 恢复 dsh 默认。
///
/// 必须重启进程才生效：权限挂在 `DSH_PERMISSION_MODE` 上，只在 spawn 时读一次。
/// 这里直接关掉当前连接，下一次发消息会懒启动一个新的——正在跑的回合会断，
/// 所以前端要在改之前把"会打断当前对话"说清楚。
#[tauri::command]
pub async fn dsh_set_permission(
    runtime: tauri::State<'_, DshRuntime>,
    mode: Option<String>,
) -> Result<DshPermissionView, String> {
    let mode = match mode {
        None => None,
        Some(value) => Some(
            dsh::permission::Mode::parse(&value)
                .ok_or_else(|| format!("未知的权限档位：{value}"))?,
        ),
    };
    dsh::permission::save(mode)?;
    runtime.shutdown();
    Ok(dsh_permission_view())
}

// ---------------------------------------------------------------- 原生引擎

/// 原生引擎的模型服务清单（不含密钥原文）。
#[tauri::command]
pub async fn native_config() -> Result<native::config::NativeConfigView, String> {
    Ok(native::config::load().view())
}

/// 新增 / 更新一个模型服务；`apiKey` 留空表示沿用已存的密钥。
#[tauri::command]
pub async fn native_provider_save(
    provider: native::config::ProviderInput,
) -> Result<native::config::NativeConfigView, String> {
    tauri::async_runtime::spawn_blocking(move || native::config::save_provider(provider))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 删除一个模型服务（选中的服务被删会自动改选别家）。
#[tauri::command]
pub async fn native_provider_delete(
    id: String,
) -> Result<native::config::NativeConfigView, String> {
    tauri::async_runtime::spawn_blocking(move || native::config::delete_provider(&id))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 选默认模型。
#[tauri::command]
pub async fn native_model_select(
    provider: String,
    model: String,
) -> Result<native::config::NativeConfigView, String> {
    tauri::async_runtime::spawn_blocking(move || native::config::select_model(&provider, &model))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 开关一个原生引擎工具（落盘，下一轮生效）。
///
/// 关掉的工具不进 schema（模型看不到、也不占 token），执行侧还会兜底拦一道。
/// 只记显式值：没动过的工具按登记项自带的默认（内置开、外部工具看 trusted）。
#[tauri::command]
pub async fn native_tool_toggle(
    name: String,
    enabled: bool,
) -> Result<native::config::NativeConfigView, String> {
    tauri::async_runtime::spawn_blocking(move || native::config::set_tool(&name, enabled))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 工具清单：内置的 + 插件运行时注册的，统一一份视图。
///
/// `owner` 是外部工具的来源（`mcp.<server>`），内置工具为空——设置页据此
/// 分区显示，也让"这个工具是谁塞进来的"有答案。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolView {
    pub name: String,
    pub origin: String,
    pub permission: String,
    pub enabled: bool,
    pub owner: Option<String>,
    pub description: String,
    /// 领域标签（`bin:static` / `artifact` …）。设置页只用前两个做标记。
    pub tags: Vec<String>,
    /// read / write / exec / attack。
    pub risk: String,
    /// none / outbound / isolated。
    pub network: String,
    /// code / malware；没有沙箱为 None。
    pub sandbox: Option<String>,
}

#[tauri::command]
pub async fn tool_list() -> Result<Vec<ToolView>, String> {
    let enabled = native::tools::EnabledTools::from_config();
    let mut views: Vec<ToolView> = Vec::new();

    for def in native::tools::builtin_tools() {
        views.push(tool_view(def, &enabled, None));
    }
    for def in native::tools::ToolRegistry::global().dynamic() {
        let owner = native::tools::ToolRegistry::global().owner_of(&def.name);
        views.push(tool_view(&def, &enabled, owner));
    }
    Ok(views)
}

fn tool_view(
    def: &native::tools::ToolDef,
    enabled: &native::tools::EnabledTools,
    owner: Option<String>,
) -> ToolView {
    ToolView {
        name: def.name.to_string(),
        origin: def.origin.as_str().to_string(),
        permission: match def.permission {
            native::tools::Permission::ReadOnly => "read-only",
            native::tools::Permission::WriteWorkspace => "write",
        }
        .to_string(),
        enabled: enabled.is_on(def),
        owner,
        // 给设置页的一行简介，不是 schema 里给模型看的长描述：
        // 一格只有一百来像素，那句"读取工作目录里的一个文本文件，带行号"放不下。
        description: def.blurb.to_string(),
        tags: def.meta.tags.iter().map(|tag| tag.to_string()).collect(),
        risk: def.meta.risk.as_str().to_string(),
        network: def.meta.network.as_str().to_string(),
        sandbox: def.meta.sandbox.map(|sandbox| sandbox.as_str().to_string()),
    }
}

/// 只改运行参数（最大工具往返次数）。
#[tauri::command]
pub async fn native_config_set(
    limits: native::config::LimitsInput,
) -> Result<native::config::NativeConfigView, String> {
    tauri::async_runtime::spawn_blocking(move || native::config::update_limits(limits))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 拉取某个服务可用的模型（`GET {base}/models`）。
///
/// 三个入参都可由未保存的表单直接提供：新建服务时用户还没点保存，
/// 也要能先「拉取模型」。`apiKey` 空则用这个服务已存的密钥。
#[tauri::command]
pub async fn native_models_fetch(
    provider_id: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
) -> Result<Vec<native::config::ModelEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let saved = provider_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|id| {
                let config = native::config::load();
                config.find(id).map(|item| item.clone())
            });
        let base = base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| saved.as_ref().map(|item| item.base_url.clone()))
            .ok_or_else(|| "先把 Base URL 填上".to_string())?;
        let key = api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| saved.as_ref().map(|item| item.api_key.clone()))
            .unwrap_or_default();
        // 用临时服务对象复用同一套 URL 拼接规则（表单里的值还没落盘）。
        let draft = native::config::ProviderConfig {
            id: saved
                .as_ref()
                .map(|item| item.id.clone())
                .unwrap_or_else(|| "adhoc".to_string()),
            label: saved
                .as_ref()
                .map(|item| item.label.clone())
                .unwrap_or_default(),
            protocol: "openai".to_string(),
            base_url: base,
            api_key: key,
            models: Vec::new(),
        };
        native::provider::fetch_models(&draft)
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 连通性探测：发一条最小请求验证 地址 / 密钥 / 模型。
#[tauri::command]
pub async fn native_probe(
    provider_id: Option<String>,
    model: Option<String>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let config = native::config::load();
        let picked = match provider_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(id) => {
                let item = config
                    .find(id)
                    .ok_or_else(|| format!("模型服务不存在：{id}"))?;
                let model = model
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .or_else(|| item.models.first().map(|entry| entry.id.clone()))
                    .ok_or_else(|| format!("{} 还没勾选模型", item.label))?;
                Some((item.clone(), model))
            }
            None => None,
        };
        let (provider, model) = match picked {
            Some(pair) => pair,
            None => {
                let (provider, model) = config.selection().ok_or_else(|| {
                    "还没有可用的模型服务：先去 设置 → 模型管理 添加一个".to_string()
                })?;
                (provider.clone(), model.id.clone())
            }
        };
        native::provider::probe(&provider, &model)
    })
    .await
    .map_err(|cause| cause.to_string())?
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
        items.extend(native::store::list_projects());
        // dsh 的项目直接由日志目录聚合：不需要拉起 ACP 进程，也不会因为
        // 引擎没启动而漏掉历史。
        items.extend(dsh::history::list_projects(500));
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
    if native::is_native_project(&project_id) {
        let limit = limit.unwrap_or(500).clamp(1, 1000) as usize;
        return tauri::async_runtime::spawn_blocking(move || {
            native::store::list_sessions(&project_id, limit)
        })
        .await
        .map_err(|cause| cause.to_string());
    }

    if dsh::is_dsh_project(&project_id) {
        let limit = limit.unwrap_or(500).clamp(1, 1000) as usize;
        return tauri::async_runtime::spawn_blocking(move || {
            dsh::history::list_sessions(&project_id, limit)
        })
        .await
        .map_err(|cause| cause.to_string());
    }

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
    if native::is_native_session(&session_id) {
        return Ok(native::store::summary(&session_id));
    }
    if dsh::is_dsh_session(&session_id) {
        return tauri::async_runtime::spawn_blocking(move || dsh::history::session_summary(&session_id))
            .await
            .map_err(|cause| cause.to_string());
    }
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
    tauri::async_runtime::spawn_blocking(move || opencode::db::get_session(&db_path, &session_id))
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

    if native::is_native_session(&session_id) {
        return tauri::async_runtime::spawn_blocking(move || native::store::timeline(&session_id))
            .await
            .map_err(|cause| cause.to_string());
    }

    if dsh::is_dsh_session(&session_id) {
        let page = tauri::async_runtime::spawn_blocking(move || {
            dsh::history::read_timeline(&session_id, before_seq, limit)
        })
        .await
        .map_err(|cause| cause.to_string())?;
        let page = page?;
        #[cfg(debug_assertions)]
        println!(
            "[prism] read_timeline(dsh) -> {} 条 (has_more={})",
            page.records.len(),
            page.has_more
        );
        return Ok(page);
    }
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
    if native::is_native_session(&session_id) {
        let detail = native::store::tool_detail(&session_id, &call_id);
        return detail.ok_or_else(|| format!("这条会话里找不到工具调用 {call_id}"));
    }
    if dsh::is_dsh_session(&session_id) {
        return tauri::async_runtime::spawn_blocking(move || {
            dsh::history::read_tool_detail(&session_id, &call_id)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }
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
///
/// 走引擎注册表分发：不再按引擎名写 match 分支。`opencode` 是缺省引擎，
/// 保持与旧行为一致（`engine` 为 None 时用它）。
#[tauri::command]
pub async fn session_create(
    registry: tauri::State<'_, crate::engines::engine::RegistryState>,
    directory: String,
    engine: Option<String>,
) -> Result<CreatedSession, String> {
    let engine_id = engine.unwrap_or_else(|| "opencode".to_string());
    #[cfg(debug_assertions)]
    println!("[prism] session_create -> {directory}（engine={engine_id}）");

    let target = registry
        .lock()
        .map_err(|cause| cause.to_string())?
        .get(&engine_id)
        .ok_or_else(|| format!("该引擎（{engine_id}）的运行轴尚在开发，暂不能新建会话"))?;

    let created = tauri::async_runtime::spawn_blocking(move || target.create_session(&directory))
        .await
        .map_err(|cause| cause.to_string())??;
    Ok(CreatedSession {
        session_id: created.session_id,
        directory: created.directory,
    })
}

/// 组合器发送的可选项（不传 = 普通纯文本消息）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendOptions {
    /// 文件附件（uri 为绝对路径；Codex 侧折算为文本提示）。
    #[serde(default)]
    pub files: Vec<FileRef>,
    /// 模型 id（OpenCode 为 modelID，Codex 为 model）。
    #[serde(default)]
    pub model: Option<String>,
    /// 模型提供方（仅 OpenCode 需要）。
    #[serde(default)]
    pub provider: Option<String>,
    /// 推理档位（OpenCode variant / Codex effort）。
    #[serde(default)]
    pub variant: Option<String>,
    /// 权限档位（仅原生引擎：read-only / ask / full）。
    #[serde(default)]
    pub approval: Option<String>,
    /// 图片附件（base64；只有原生引擎收，且要模型开了「图片」开关）。
    #[serde(default)]
    pub images: Vec<crate::engines::native::agent::IncomingImage>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRef {
    pub uri: String,
    #[serde(default)]
    pub name: String,
}

/// Codex 无结构化附件：折算成文本里的附件清单（模型会自行读取）。
fn files_to_note(text: &str, files: &[FileRef]) -> String {
    if files.is_empty() {
        return text.to_string();
    }
    let mut out = text.trim_end().to_string();
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str("[Prism 附件] 请阅读以下文件：");
    for file in files {
        if file.name.trim().is_empty() {
            out.push_str(&format!("\n- {}", file.uri));
        } else {
            out.push_str(&format!("\n- {} （{}）", file.uri, file.name));
        }
    }
    out
}

/// 发送一条用户消息，返回用户消息 id（Codex 无此 id，返回空串）。
/// options 可带文件附件 / 模型 / 推理档位（组合器）。
#[tauri::command]
pub async fn session_send(
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Runtime>,
    codex_runtime: tauri::State<'_, CodexRuntime>,
    dsh_runtime: tauri::State<'_, DshRuntime>,
    session_id: String,
    text: String,
    options: Option<SendOptions>,
) -> Result<String, String> {
    let options = options.unwrap_or_default();
    #[cfg(debug_assertions)]
    println!(
        "[prism] session_send -> {}（{} 字符，附件 {}，模型 {:?}）",
        session_id,
        text.chars().count(),
        options.files.len(),
        options.model
    );
    if native::is_native_session(&session_id) {
        // 原生引擎自己跑循环：流式增量走 engine-event，落定后写自己的 JSONL。
        let text = files_to_note(&text, &options.files);
        return tauri::async_runtime::spawn_blocking(move || {
            native::agent::send(
                &app,
                &session_id,
                &text,
                options.provider.as_deref(),
                options.model.as_deref(),
                options.approval.as_deref(),
                &options.images,
            )
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }
    if is_codex_session(&session_id) {
        let codex_runtime = codex_runtime.inner().clone();
        let text = files_to_note(&text, &options.files);
        return tauri::async_runtime::spawn_blocking(move || {
            codex_runtime.send_prompt_with(
                &session_id,
                &text,
                options.model.as_deref(),
                options.variant.as_deref(),
            )
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }
    if dsh::is_dsh_session(&session_id) {
        let dsh_runtime = dsh_runtime.inner().clone();
        // ACP 的 prompt 只吃文本块，附件按"路径清单"折进正文。
        let text = files_to_note(&text, &options.files);
        return tauri::async_runtime::spawn_blocking(move || {
            dsh_runtime.send_prompt_with(
                &session_id,
                &text,
                options.model.as_deref(),
                options.variant.as_deref(),
            )
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(model) = options
            .model
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            let provider = options
                .provider
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "指定 OpenCode 模型时缺少提供方".to_string())?;
            runtime.set_session_model(&session_id, provider, model, options.variant.as_deref())?;
        }
        let files: Vec<(String, String)> = options
            .files
            .iter()
            .map(|file| (file.uri.clone(), file.name.clone()))
            .collect();
        runtime.send_prompt_rich(&session_id, &text, &files)
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 引擎能力目录：模型（含推理档位）+ 斜杠命令（组合器数据源）。
///
/// 同样走注册表分发——每个引擎自己能拿到什么目录，由它的 `Engine::catalog`
/// 决定；未知引擎落回"不可用"而不是报错（前端组合器按 available 降级）。
#[tauri::command]
pub async fn engine_catalog(
    registry: tauri::State<'_, crate::engines::engine::RegistryState>,
    engine: String,
    directory: String,
) -> Result<EngineCatalog, String> {
    let target = registry
        .lock()
        .map_err(|cause| cause.to_string())?
        .get(&engine);
    let Some(target) = target else {
        return Ok(catalog::unavailable(&engine, format!("未知引擎：{engine}")));
    };
    tauri::async_runtime::spawn_blocking(move || target.catalog(&directory))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 在会话里直接跑一条 shell 命令（不发给模型）。
#[tauri::command]
pub async fn session_shell(
    runtime: tauri::State<'_, Runtime>,
    codex_runtime: tauri::State<'_, CodexRuntime>,
    session_id: String,
    command: String,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    println!(
        "[prism] session_shell -> {session_id}（{} 字符）",
        command.chars().count()
    );
    if is_codex_session(&session_id) {
        let codex_runtime = codex_runtime.inner().clone();
        return tauri::async_runtime::spawn_blocking(move || {
            codex_runtime.shell_command(&session_id, &command)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }
    if dsh::is_dsh_session(&session_id) {
        // ACP 没有"代跑 shell"的方法：dsh 的工具都关在自己进程里，外部控制器
        // 只能发 prompt，不能直接执行命令。
        return Err("DeepSeek Harness 不支持从 Prism 直接执行 shell".to_string());
    }
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.session_shell(&session_id, &command))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 运行一条斜杠命令（原生：/compact；OpenCode：引擎自己的命令）。
#[tauri::command]
pub async fn session_command(
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Runtime>,
    session_id: String,
    name: String,
    text: String,
) -> Result<String, String> {
    #[cfg(debug_assertions)]
    println!("[prism] session_command -> {session_id} /{name}");
    if native::is_native_session(&session_id) {
        // 原生引擎的命令是我们自己实现的：不认识的直接说清楚，
        // 别落到底下的 OpenCode runtime 上、报一句莫名其妙的会话不存在。
        if name != "compact" {
            return Err(format!("原生引擎没有 /{name}（目前内置 /compact）"));
        }
        return native::compact::compact(&app, &session_id, &text);
    }
    if is_codex_session(&session_id) {
        return Err("Codex 暂不支持斜杠命令（可直接输入自然语言指令）".to_string());
    }
    if dsh::is_dsh_session(&session_id) {
        // dsh 的命令是插件在会话里注册的，ACP 不上报全集，也没有"执行命令"的方法。
        return Err("DeepSeek Harness 暂不支持斜杠命令（可直接输入自然语言指令）".to_string());
    }
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.session_command(&session_id, &name, &text))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 项目文件索引（@ 补全）：query 为空时按路径序给首批。
#[tauri::command]
pub async fn project_files(
    directory: String,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<FileHit>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        files::list_project_files(&directory, &query, limit.unwrap_or(30))
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// Git 上下文快照（分支 / 变更 / 最近提交），供「＋上下文」快捷插入。
#[tauri::command]
pub async fn git_snapshot(directory: String) -> Result<GitSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || files::git_snapshot(&directory))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 会话目录的未提交改动（文件变更追踪）。
#[tauri::command]
pub async fn changes_list(directory: String) -> Result<crate::changes::ChangeReport, String> {
    tauri::async_runtime::spawn_blocking(move || crate::changes::report(&directory))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 单个文件的统一 diff（新文件按内容拼）。
#[tauri::command]
pub async fn changes_diff(
    directory: String,
    path: String,
) -> Result<crate::changes::FileDiff, String> {
    tauri::async_runtime::spawn_blocking(move || crate::changes::diff(&directory, &path))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 文件预览：返回文本（二进制只回标记，不解码）。
#[tauri::command]
pub async fn file_text(path: String) -> Result<crate::changes::FileText, String> {
    tauri::async_runtime::spawn_blocking(move || crate::changes::read_text(&path))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 本地分支列表（会话头部的分支切换弹层）。
#[tauri::command]
pub async fn git_branches(directory: String) -> Result<GitBranches, String> {
    tauri::async_runtime::spawn_blocking(move || files::git_branches(&directory))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 切换（或新建并切换）分支；成功后返回新的快照。
#[tauri::command]
pub async fn git_switch_branch(
    directory: String,
    branch: String,
    create: Option<bool>,
) -> Result<GitSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || {
        files::git_switch_branch(&directory, &branch, create.unwrap_or(false))
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 中断当前执行。
#[tauri::command]
pub async fn session_interrupt(
    runtime: tauri::State<'_, Runtime>,
    codex_runtime: tauri::State<'_, CodexRuntime>,
    dsh_runtime: tauri::State<'_, DshRuntime>,
    session_id: String,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    println!("[prism] session_interrupt -> {session_id}");
    if native::is_native_session(&session_id) {
        // 原生引擎的回合跑在阻塞线程里，靠一个 stop 标记在流式分片之间收手。
        return if native::agent::cancel(&session_id) {
            Ok(())
        } else {
            Err("这个会话没有正在跑的回合".to_string())
        };
    }
    if is_codex_session(&session_id) {
        let codex_runtime = codex_runtime.inner().clone();
        return tauri::async_runtime::spawn_blocking(move || codex_runtime.interrupt(&session_id))
            .await
            .map_err(|cause| cause.to_string())?;
    }
    if dsh::is_dsh_session(&session_id) {
        let dsh_runtime = dsh_runtime.inner().clone();
        return tauri::async_runtime::spawn_blocking(move || dsh_runtime.interrupt(&session_id))
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
    if native::is_native_session(&session_id) {
        return Ok(native::approval::list(&session_id));
    }
    if dsh::is_dsh_session(&session_id) {
        // dsh 的权限询问只在事件流里出现一次（`permission.asked`）。切会话补拉
        // 需要按会话 id 把挂起项捞回来，暂时没做——卡片本身就是事件驱动的，
        // 正常路径不会漏。
        return Ok(Vec::new());
    }
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
    dsh_runtime: tauri::State<'_, DshRuntime>,
    session_id: String,
    request_id: String,
    decision: String,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    println!("[prism] session_permission_reply -> {session_id} {request_id} {decision}");
    if native::is_native_session(&session_id) {
        return native::approval::reply(&request_id, &decision);
    }
    if is_codex_session(&session_id) {
        let codex_runtime = codex_runtime.inner().clone();
        return tauri::async_runtime::spawn_blocking(move || {
            codex_runtime.permission_reply(&session_id, &request_id, &decision)
        })
        .await
        .map_err(|cause| cause.to_string())?;
    }
    if dsh::is_dsh_session(&session_id) {
        let dsh_runtime = dsh_runtime.inner().clone();
        return tauri::async_runtime::spawn_blocking(move || {
            dsh_runtime.permission_reply(&session_id, &request_id, &decision)
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
/// 读取本地交付物（HTML / SVG / 图片）内容，供时间线就地预览。
#[tauri::command]
pub async fn read_artifact(path: String) -> Result<crate::artifact::ArtifactData, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::artifact::read_artifact(std::path::Path::new(&path))
    })
    .await
    .map_err(|cause| cause.to_string())?
}

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

/// 用系统默认浏览器打开一个 http(s) 地址。
///
/// 内嵌浏览器（`browser.rs`）建不出来时的退路：能看的还是能看，只是不在面板里。
#[tauri::command]
pub async fn open_url(url: String) -> Result<(), String> {
    let parsed = tauri::Url::parse(&url).map_err(|error| format!("地址不合法：{error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("只支持 http(s) 地址".to_string());
    }
    let target = parsed.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        let opened = std::process::Command::new("open").arg(&target).spawn();
        #[cfg(target_os = "windows")]
        let opened = std::process::Command::new("cmd")
            .args(["/C", "start", "", &target])
            .spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let opened = std::process::Command::new("xdg-open").arg(&target).spawn();
        opened
            .map(|_| ())
            .map_err(|cause| format!("打开系统浏览器失败：{cause}"))
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 上下文明细：系统提示 / 工具 / 对话各占多少字符（原生引擎才有）。
#[tauri::command]
pub async fn context_stats(
    session_id: String,
) -> Result<Option<crate::engines::native::context::ContextStats>, String> {
    if !native::is_native_session(&session_id) {
        return Ok(None);
    }
    tauri::async_runtime::spawn_blocking(move || {
        crate::engines::native::context::stats(&session_id)
    })
    .await
    .map_err(|cause| cause.to_string())
}

/// 这个目录现在还在不在（历史会话可能指向已经删掉 / 移动的目录）。
#[tauri::command]
pub async fn dir_exists(path: String) -> Result<bool, String> {
    Ok(crate::changes::dir_exists(&path))
}

/// 保存文本文件（编辑器用；只允许会话目录 / 仓库根之内的已存在文件）。
#[tauri::command]
pub async fn write_text(path: String, text: String, directory: String) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::changes::write_text(&path, &text, &directory)
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 在文件管理器里显示这个文件（Finder / 资源管理器 / xdg-open）。
#[tauri::command]
pub async fn reveal_path(path: String) -> Result<(), String> {
    let target = PathBuf::from(&path);
    if !target.exists() {
        return Err(format!("路径不存在：{path}"));
    }
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg("-R").arg(&path);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(format!("/select,{path}"));
        command
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(target.parent().unwrap_or(&target));
        command
    };
    command
        .spawn()
        .map_err(|cause| format!("打开失败：{cause}"))?;
    Ok(())
}

/// 读取全部档案增补（星标 / 标签 / 自定义标题）。
#[tauri::command]
pub async fn annotations_get() -> Result<AnnotationStore, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = annotations::store_path()?;
        Ok(annotations::load(&path))
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 更新单个会话的增补信息；返回全量存储（前端整体替换）。
#[tauri::command]
pub async fn annotation_update(
    session_id: String,
    title: Option<String>,
    starred: Option<bool>,
    tags: Option<Vec<String>>,
) -> Result<AnnotationStore, String> {
    tauri::async_runtime::spawn_blocking(move || {
        annotations::mutate(|store| {
            annotations::update_session(store, &session_id, title, starred, tags)
        })
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 批量关闭 / 恢复会话（只写 Prism 的增补表，引擎数据不动）。
#[tauri::command]
pub async fn annotations_set_hidden(
    session_ids: Vec<String>,
    hidden: bool,
) -> Result<AnnotationStore, String> {
    tauri::async_runtime::spawn_blocking(move || {
        annotations::mutate(|store| {
            annotations::set_hidden(store, &session_ids, hidden);
            Ok(())
        })
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 新建标签。
#[tauri::command]
pub async fn tag_create(name: String, color: String) -> Result<AnnotationStore, String> {
    tauri::async_runtime::spawn_blocking(move || {
        annotations::mutate(|store| annotations::create_tag(store, &name, &color).map(|_| ()))
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 重命名 / 换色标签。
#[tauri::command]
pub async fn tag_update(
    id: String,
    name: Option<String>,
    color: Option<String>,
) -> Result<AnnotationStore, String> {
    tauri::async_runtime::spawn_blocking(move || {
        annotations::mutate(|store| annotations::update_tag(store, &id, name, color))
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 删除标签（同时摘掉所有会话上的引用）。
#[tauri::command]
pub async fn tag_delete(id: String) -> Result<AnnotationStore, String> {
    tauri::async_runtime::spawn_blocking(move || {
        annotations::mutate(|store| annotations::delete_tag(store, &id))
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 合并标签（来源 → 目标）。
#[tauri::command]
pub async fn tag_merge(source_id: String, target_id: String) -> Result<AnnotationStore, String> {
    tauri::async_runtime::spawn_blocking(move || {
        annotations::mutate(|store| annotations::merge_tag(store, &source_id, &target_id))
    })
    .await
    .map_err(|cause| cause.to_string())?
}

/// 批量查询目录的 git 信息（分支 / 主仓库根 / worktree 标记）。
#[tauri::command]
pub async fn git_info_batch(
    paths: Vec<String>,
) -> Result<std::collections::HashMap<String, crate::gitinfo::GitInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || crate::gitinfo::git_info_batch(&paths))
        .await
        .map_err(|cause| cause.to_string())
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
pub async fn routines_list(
    state: tauri::State<'_, RoutinesState>,
) -> Result<Vec<RoutineView>, String> {
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
    println!(
        "[prism] routine_create -> {}（{}）",
        definition.name, definition.id
    );
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
pub async fn routine_logs(
    id: String,
    limit: Option<usize>,
) -> Result<Vec<RoutineLogEntry>, String> {
    let dir = store::logs_dir().ok_or_else(|| "无法定位 ~/.prism 数据目录".to_string())?;
    Ok(store::read_logs(&dir, &id, limit.unwrap_or(20)))
}

/// 托盘状态刷新：前端把「执行中 / 待审批」实时推给菜单栏（尽力而为，失败静默）。
#[tauri::command]
pub fn tray_status(app: tauri::AppHandle, running: u32, waiting: u32) {
    crate::tray::update_status(&app, running, waiting);
}

// ---- 后台任务（M23）----
//
// 会话之外独立跑的进程：构建 / 测试 / 脚本。注册表在 `tasks::TasksState`，
// 输出与状态通过 `task-output` / `task-state` 事件推给前端。

/// 起一个后台任务；`background = false` 表示只进终端页、不占任务坞。
#[tauri::command]
pub async fn task_spawn(
    app: tauri::AppHandle,
    tasks: tauri::State<'_, TasksState>,
    cmd: String,
    cwd: String,
    session_id: Option<String>,
    title: Option<String>,
    background: Option<bool>,
) -> Result<TaskInfo, String> {
    #[cfg(debug_assertions)]
    println!(
        "[prism] task_spawn -> {}（{}）",
        cmd.chars().take(60).collect::<String>(),
        cwd
    );
    tasks.inner().spawn(
        app,
        SpawnRequest {
            cmd,
            cwd,
            session_id,
            title,
            background: background.unwrap_or(true),
            origin: "user".to_string(),
        },
    )
}

/// 全部任务（新起的在前）。
#[tauri::command]
pub fn task_list(tasks: tauri::State<'_, TasksState>) -> Vec<TaskInfo> {
    tasks.list()
}

/// 停止一个任务（整组信号）。
#[tauri::command]
pub fn task_stop(tasks: tauri::State<'_, TasksState>, id: String) -> Result<(), String> {
    tasks.stop(&id)
}

/// 把已结束的任务移出列表。
#[tauri::command]
pub fn task_remove(tasks: tauri::State<'_, TasksState>, id: String) -> Result<(), String> {
    tasks.remove(&id)
}

// ---------------------------------------------------------------- Agent 注册表

/// 全部 Agent：内置 presets + 用户自定义（同 id 时 user 覆盖 builtin）。
#[tauri::command]
pub fn agents_list() -> Result<Vec<AgentProfile>, String> {
    Ok(agents::list())
}

/// 保存一个 Agent（新建或覆盖）。同 id 的内置 preset 会被覆盖，删掉即还原。
#[tauri::command]
pub fn agent_save(profile: AgentProfile) -> Result<AgentProfile, String> {
    agents::save(profile)
}

/// 删除自定义 Agent。内置 Agent 没被覆盖时不可删（它不在磁盘上）。
#[tauri::command]
pub fn agent_delete(id: String) -> Result<(), String> {
    agents::delete(&id)
}

// ---------------------------------------------------------------- DAG 编排

/// 计划列表（最近改过的在前）。
#[tauri::command]
pub fn dag_list() -> Result<Vec<DagSummary>, String> {
    Ok(dag::store::list())
}

/// 单张计划。
#[tauri::command]
pub fn dag_get(id: String) -> Result<DagPlan, String> {
    dag::store::get(&id)
}

/// 空白草稿（带一个新 id）。手动搭图从这里开始；Planner 那条路走 `dag_plan`。
#[tauri::command]
pub fn dag_new(project: String) -> Result<DagPlan, String> {
    Ok(dag::planner::blank(&project))
}

/// 保存计划（草案也允许存：编辑中途的半成品不该被拦）。
///
/// 校验是**独立**的一条命令：`dag_validate`。这样"存不下"只代表磁盘问题，
/// "跑不了"才是图的问题——两者混在一起会让用户不知道该改哪里。
#[tauri::command]
pub fn dag_save(plan: DagPlan) -> Result<DagPlan, String> {
    dag::store::save(plan)
}

#[tauri::command]
pub fn dag_delete(id: String) -> Result<(), String> {
    dag::store::delete(&id)
}

/// 校验一张（可能还没保存的）计划。
#[tauri::command]
pub fn dag_validate(plan: DagPlan) -> Result<Vec<Issue>, String> {
    let known: Vec<String> = agents::list().into_iter().map(|agent| agent.id).collect();
    Ok(dag::validate::validate(&plan, &known))
}

/// 某个 DAG 的制品清单（新的在前）+ 已用空间。
///
/// 编排页的「产物」用它：用户在跑完之后要能看见"这个节点到底产了什么"。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DagArtifactList {
    pub items: Vec<dag::artifact::ArtifactRef>,
    pub bytes: u64,
}

#[tauri::command]
pub fn dag_artifacts(dag_id: String) -> Result<DagArtifactList, String> {
    let store = dag::artifact::ArtifactStore::global()?;
    Ok(DagArtifactList {
        items: store.list(&dag_id),
        bytes: store.dag_bytes(&dag_id),
    })
}

/// 制品正文（预览用）。`limit` 是字符数，默认 40 000。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DagArtifactText {
    pub artifact: dag::artifact::ArtifactRef,
    pub text: String,
    pub truncated: bool,
}

#[tauri::command]
pub fn dag_artifact_get(
    dag_id: String,
    id: String,
    limit: Option<usize>,
) -> Result<DagArtifactText, String> {
    let store = dag::artifact::ArtifactStore::global()?;
    let limit = limit.unwrap_or(40_000).clamp(1, 200_000);
    let (artifact, text, truncated) = store.read_text(&dag_id, &id, limit)?;
    Ok(DagArtifactText {
        artifact,
        text,
        truncated,
    })
}

/// 让 Planner 从一句目标生成草案（**不落盘**，确认后再 `dag_save`）。
///
/// 模型调用是阻塞的，丢到 blocking 线程池：拆一张图要十几秒，不能占住
/// Tauri 的主线程，也不该堵 async runtime 的 worker。
#[tauri::command]
pub async fn dag_plan(
    goal: String,
    project: String,
    max_nodes: Option<usize>,
) -> Result<DagPlan, String> {
    let agents = agents::list();
    let max = max_nodes.unwrap_or(dag::planner::DEFAULT_MAX_NODES);
    tauri::async_runtime::spawn_blocking(move || {
        dag::planner::draft(&goal, &project, &agents, max)
    })
    .await
    .map_err(|error| format!("规划线程异常退出：{error}"))?
}

// ---------------------------------------------------------------- 执行引擎

/// 开跑一张计划。
///
/// 只等"已经开跑"就返回：整张图可能跑几十分钟，IPC 不能挂在那儿。之后
/// 状态靠 `dag.run` 事件推（载荷是整份快照），前端不用轮询。
///
/// 落盘的计划才允许跑：运行记录要能回指到一张固定的图，否则"跑到一半改了
/// 图"会让结果对不上。
#[tauri::command]
pub fn dag_run(
    app: tauri::AppHandle,
    hub: tauri::State<'_, dag::DagHub>,
    id: String,
) -> Result<dag::DagRun, String> {
    use tauri::Emitter;

    let plan = dag::store::get(&id)?;
    let agents = agents::list();
    // 执行器由插件注册（`dag.executor.*`）：装没装、哪个 Agent 缺，一次说清。
    // 一个都没装时也走这条路——报错文案就在 preflight 里。
    dag::executor::preflight(&plan, &agents)?;
    let snapshot_app = app.clone();
    let notify = std::sync::Arc::new(move |run: &dag::DagRun| {
        let _ = snapshot_app.emit(dag::run::EVENT_NAME, run);
    });
    // 增量（正文 / 步骤 / token）走单独一条：快照太大，逐 delta 推会把 IPC 打满。
    let live_app = app.clone();
    let live: dag::run::LiveNotifier = std::sync::Arc::new(move |event: &dag::run::NodeLive| {
        let _ = live_app.emit(dag::run::LIVE_EVENT_NAME, event);
    });
    hub.start_with(plan, &agents, None, notify, live)
}

/// 已装的节点执行器 id（Agent 编辑器据此把"跑不了"的引擎标出来）。
#[tauri::command]
pub fn dag_runners() -> Vec<String> {
    dag::runner::installed()
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// 停止一次运行。正在跑的节点会在下一次工具/模型调用前收手。
#[tauri::command]
pub fn dag_run_cancel(
    hub: tauri::State<'_, dag::DagHub>,
    run_id: String,
) -> Result<dag::DagRun, String> {
    let handle = hub
        .get(&run_id)
        .ok_or_else(|| format!("没有这次运行：{run_id}"))?;
    handle.cancel()
}

/// 人工闸门的答复。`allowed=false` 表示"别跑了"，节点标取消、下游标跳过。
#[tauri::command]
pub fn dag_run_approve(
    hub: tauri::State<'_, dag::DagHub>,
    run_id: String,
    node_id: String,
    allowed: bool,
) -> Result<dag::DagRun, String> {
    let handle = hub
        .get(&run_id)
        .ok_or_else(|| format!("没有这次运行：{run_id}"))?;
    handle.approve(&node_id, allowed)
}

/// 某张计划最近一次运行（内存里没有就从磁盘捡）。
#[tauri::command]
pub fn dag_run_latest(
    hub: tauri::State<'_, dag::DagHub>,
    plan_id: String,
) -> Result<Option<dag::DagRun>, String> {
    Ok(hub.latest_run(&plan_id))
}

/// 按 id 取一次运行（刷新页面 / 重启后仍能看历史）。
#[tauri::command]
pub fn dag_run_get(
    hub: tauri::State<'_, dag::DagHub>,
    run_id: String,
) -> Result<Option<dag::DagRun>, String> {
    Ok(hub.run_by_id(&run_id))
}

// ---------------------------------------------------------------- 插件清单

/// 本次装配的插件清单（含被配置关掉 / 级联关掉的项）。
#[tauri::command]
pub fn plugins_list(kernel: tauri::State<'_, Mutex<KernelState>>) -> Result<Vec<PluginInfo>, String> {
    let state = kernel.lock().map_err(|_| "内核状态被占用".to_string())?;
    Ok(state.manifest())
}

/// 开关一个插件。**下次启动生效**——装配只在启动时跑一次。
#[tauri::command]
pub fn plugin_set_enabled(
    kernel: tauri::State<'_, Mutex<KernelState>>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    {
        let state = kernel.lock().map_err(|_| "内核状态被占用".to_string())?;
        if !state.manifest().iter().any(|item| item.id == id) {
            return Err(format!("没有这个插件：{id}"));
        }
    }
    kernel::set_enabled(&id, enabled)
}

// ---------------------------------------------------------------- 面板浏览器
//
// 一块挂在主窗口上的真 webview（见 `browser.rs`）。前端负责把内容区矩形报上来、
// 在浮层出现时藏起来；Agent 的 `web.*` 工具复用同一块页面。

/// 打开 / 切换地址（不存在就先创建那块 webview）。
#[tauri::command]
pub async fn browser_open(
    app: tauri::AppHandle,
    url: String,
) -> Result<crate::browser::BrowserSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || crate::browser::open(&app, &url))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 同步位置与尺寸（逻辑像素，相对窗口客户区）。
#[tauri::command]
pub async fn browser_bounds(
    app: tauri::AppHandle,
    bounds: crate::browser::Bounds,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || crate::browser::set_bounds(&app, bounds))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 显示 / 隐藏。原生视图会盖住 DOM，弹窗出现时必须隐藏。
#[tauri::command]
pub async fn browser_visible(app: tauri::AppHandle, visible: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || crate::browser::set_visible(&app, visible))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 后退 / 前进 / 刷新。
#[tauri::command]
pub async fn browser_history(app: tauri::AppHandle, direction: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || crate::browser::history(&app, &direction))
        .await
        .map_err(|cause| cause.to_string())?
}

/// 当前状态（面板挂载时用它对齐，不必等事件）。
#[tauri::command]
pub fn browser_state() -> crate::browser::BrowserSnapshot {
    crate::browser::snapshot()
}
