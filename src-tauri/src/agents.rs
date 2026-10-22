//! Agent 注册表：内置 presets + 用户自定义 profile。
//!
//! 定位
//! ----
//! 此前 Agent 只是 `native/subagent.rs` 里的两个枚举分支（`explore` / `general`），
//! 想知道"有哪些 Agent、各自能用什么工具"只能读代码。这里把它抬成**数据**：
//!
//! ```text
//! ~/.prism/agents/user/<id>.json     用户自定义（可改、可删）
//! 代码里的 builtin()                  内置 presets（升级即覆盖，不会被改坏）
//! ```
//!
//! 合并规则只有一条：**同 id 时 user 覆盖 builtin**，删掉 user 文件即还原内置。
//! 这样"定制"和"还原"都不需要额外的开关，也不需要备份原文件。
//!
//! Agent 是配置不是代码：这里只管读写与校验，真正拿它跑会话的是执行器
//! （`dag.executor.*`，下一里程碑）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// id 最长多少字符（文件名就是 id，太长了在界面上也放不下）。
const ID_MAX: usize = 32;

/// profile 从哪来。UI 用它区分「内置」与「自定义」角标。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// 代码里的 preset。
    #[default]
    Builtin,
    /// `~/.prism/agents/user/` 下的文件。
    User,
}

/// 权限档位。与 dsh 的三档同名，语义对齐（便于执行器直接透传环境变量）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    /// 只能看：写文件与跑命令一律拒绝。
    ReadOnly,
    /// 只能改工作目录（默认：最小可用且不吓人）。
    #[default]
    WorkspaceWrite,
    /// 全盘可写、从不询问。
    FullAccess,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::ReadOnly => "read-only",
            Permission::WorkspaceWrite => "workspace-write",
            Permission::FullAccess => "danger-full-access",
        }
    }
}

/// 节点默认在哪种工作区里跑（DAG 节点可以覆盖）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Workspace {
    /// 直接在项目目录跑。只读调研、单写者串行链。
    #[default]
    Shared,
    /// 独立 git worktree + 分支。并行写、需要 diff。
    Worktree,
    /// 空目录，产物只能通过 report / patch 带出。
    Scratch,
}

/// 工具策略：公用白名单 + 私有 + 显式拒绝。
///
/// `shared: None` 是"该 Agent 默认可见的全部公用工具"，不是"没有工具"——
/// 空数组才是"一个公用工具都不要"。这个区别必须保留：绝大多数 Agent 不该被
/// 逼着把内置工具名逐条抄一遍（工具表会变，抄下来就会腐坏）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSelection {
    #[serde(default)]
    pub shared: Option<Vec<String>>,
    #[serde(default)]
    pub private: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// 一个 Agent 的完整画像。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 由读写路径决定，不信任文件里的值（会被覆盖）。
    #[serde(default)]
    pub origin: Origin,
    /// 这个 profile 盖掉了一个同名内置 preset（UI 显示「覆盖内置」）。
    #[serde(default)]
    pub overrides_builtin: bool,
    /// 执行器：`native` 或引擎 id（`dsh` / `codex` / `opencode` / `claude`）。
    #[serde(default = "default_executor")]
    pub executor: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub permission: Permission,
    #[serde(default)]
    pub workspace: Workspace,
    #[serde(default)]
    pub tools: ToolSelection,
    /// 引用 `~/.prism/mcp.json` 的 server id（执行器按需懒启动）。
    #[serde(default)]
    pub mcp: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// worktree 缺依赖时从主仓 symlink 过去的目录（如 `node_modules`）。
    #[serde(default)]
    pub link_dirs: Vec<String>,
    /// 该 Agent 的节点默认要人工确认。
    #[serde(default)]
    pub gate_default: bool,
}

fn default_executor() -> String {
    "native".to_string()
}

fn tools(shared: &[&str], private: &[&str], deny: &[&str]) -> ToolSelection {
    ToolSelection {
        shared: Some(shared.iter().map(|item| item.to_string()).collect()),
        private: private.iter().map(|item| item.to_string()).collect(),
        deny: deny.iter().map(|item| item.to_string()).collect(),
    }
}

fn preset(
    id: &str,
    name: &str,
    description: &str,
    executor: &str,
    permission: Permission,
    workspace: Workspace,
    system_prompt: &str,
    tags: &[&str],
    selection: ToolSelection,
) -> AgentProfile {
    AgentProfile {
        id: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        origin: Origin::Builtin,
        overrides_builtin: false,
        executor: executor.to_string(),
        model: None,
        system_prompt: system_prompt.to_string(),
        permission,
        workspace,
        tools: selection,
        mcp: Vec::new(),
        tags: tags.iter().map(|item| item.to_string()).collect(),
        link_dirs: if workspace == Workspace::Worktree {
            vec!["node_modules".to_string()]
        } else {
            Vec::new()
        },
        gate_default: false,
    }
}

/// 内置 presets。顺序即 UI 展示顺序。
///
/// `explore` / `general` 与 `native/subagent.rs` 的 `AgentType` 一一对应：
/// 老会话里的 `task` 工具调用（`agent_type: "explore"`）解析结果不会因为
/// 这次改动而变。
pub fn builtin() -> Vec<AgentProfile> {
    vec![
        preset(
            "explore",
            "探索者",
            "只读调研：定位文件、梳理调用链，结论必须带「文件:行号」证据。",
            "native",
            Permission::ReadOnly,
            Workspace::Shared,
            "你负责只读调研。先搜再读，结论必须有「文件:行号」证据；不修改任何文件。",
            &["调研"],
            tools(&["read_file", "grep", "list_files", "list_sessions"], &[], &[]),
        ),
        preset(
            "general",
            "通用助手",
            "默认 Agent：可读改文件、跑命令，处理没有专属 Agent 的杂活。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Shared,
            "你是一个通用工程助手。先读再改，保持仓库现有风格。",
            &["通用"],
            tools(
                &["read_file", "grep", "list_files", "edit_file", "run_command"],
                &[],
                &[],
            ),
        ),
        preset(
            "frontend",
            "前端工程师",
            "前端实现：组件、样式、交互，保持现有设计令牌与间距体系。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Worktree,
            "你负责前端实现。改动保持仓库现有风格，不引入新组件库；先读再改。",
            &["前端", "实现"],
            tools(
                &["read_file", "edit_file", "run_command", "grep"],
                &[],
                &[],
            ),
        ),
        preset(
            "backend",
            "后端工程师",
            "后端实现：Rust / IPC / 命令接线，落盘与事件语义对齐现有约定。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Worktree,
            "你负责后端实现。命令薄壳、逻辑进模块，事件信封与现有约定一致。",
            &["后端", "实现"],
            tools(
                &["read_file", "edit_file", "run_command", "grep"],
                &[],
                &[],
            ),
        ),
        preset(
            "architect",
            "架构师",
            "设计接口、状态机与数据模型，产出契约与取舍说明，不写实现。",
            "native",
            Permission::ReadOnly,
            Workspace::Shared,
            "你负责架构设计。产出契约、状态机与取舍，不改文件。",
            &["设计"],
            tools(&["read_file", "grep", "list_files"], &[], &[]),
        ),
        preset(
            "docs",
            "文档工程师",
            "文档与说明：README、变更说明、迁移指引，以实现为准。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Worktree,
            "你负责文档。以实际实现为准，不夸大、不臆造。",
            &["文档"],
            tools(&["read_file", "edit_file", "grep"], &[], &[]),
        ),
        preset(
            "verifier",
            "验证者",
            "合并并验证：跑构建与测试，输出结论、耗时与冲突。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Worktree,
            "你负责验证。跑构建与测试，输出可复现的结论。",
            &["验证"],
            tools(&["read_file", "run_command", "grep"], &[], &[]),
        ),
        preset(
            "dsh-research",
            "DeepSeek 调研",
            "用 DeepSeek Harness 做只读调研，沙箱内跑，适合不可信任务。",
            "dsh",
            Permission::ReadOnly,
            Workspace::Shared,
            "只读调研，输出证据与结论。",
            &["调研", "沙箱"],
            tools(&["read_file", "grep", "list_files"], &[], &[]),
        ),
    ]
}

/// 用户 profile 目录（需要时创建）。
pub fn user_dir() -> Option<PathBuf> {
    let dir = crate::engines::opencode::search::prism_dir()?.join("agents/user");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 校验并规范化 id：小写、数字、连字符；必须字母数字开头；不允许路径分隔符。
///
/// id 会直接当文件名用，这条校验同时是**路径穿越防线**——`../x` 之类的输入
/// 在这里被拒掉，不需要在写入前再拼一次白名单。
fn normalize_id(raw: &str) -> Result<String, String> {
    let id = raw.trim().to_ascii_lowercase();
    if id.is_empty() {
        return Err("id 不能为空".to_string());
    }
    if id.chars().count() > ID_MAX {
        return Err(format!("id 最长 {ID_MAX} 个字符"));
    }
    if id.chars().count() < 2 {
        return Err("id 至少 2 个字符".to_string());
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap_or(' ');
    if !first.is_ascii_alphanumeric() {
        return Err("id 必须以字母或数字开头".to_string());
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
        return Err("id 只能包含字母、数字、连字符与下划线".to_string());
    }
    Ok(id)
}

/// 去空、去重（保序）。
fn tidy(list: &mut Vec<String>) {
    let mut seen: Vec<String> = Vec::with_capacity(list.len());
    list.retain_mut(|item| {
        let trimmed = item.trim().to_string();
        if trimmed.is_empty() || seen.contains(&trimmed) {
            return false;
        }
        seen.push(trimmed.clone());
        *item = trimmed;
        true
    });
}

/// 规范化后写入：id / 名称 / 各列表都收一遍。
fn normalize(mut profile: AgentProfile) -> Result<AgentProfile, String> {
    profile.id = normalize_id(&profile.id)?;
    profile.name = profile.name.trim().to_string();
    if profile.name.is_empty() {
        return Err("名称不能为空".to_string());
    }
    profile.description = profile.description.trim().to_string();
    profile.executor = profile.executor.trim().to_string();
    if profile.executor.is_empty() {
        profile.executor = default_executor();
    }
    profile.model = profile
        .model
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    tidy(&mut profile.mcp);
    tidy(&mut profile.tags);
    tidy(&mut profile.link_dirs);
    tidy(&mut profile.tools.private);
    tidy(&mut profile.tools.deny);
    if let Some(shared) = profile.tools.shared.as_mut() {
        tidy(shared);
    }
    // 明确拒绝的工具不该同时出现在允许列表里：deny 优先，留着只会让
    // 「模型看到的清单」和配置对不上。
    let denied = profile.tools.deny.clone();
    if let Some(shared) = profile.tools.shared.as_mut() {
        shared.retain(|name| !denied.contains(name));
    }
    profile
        .tools
        .private
        .retain(|name| !denied.contains(name));
    Ok(profile)
}

fn read_profile(path: &Path) -> Result<AgentProfile, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    serde_json::from_str::<AgentProfile>(&text)
        .map_err(|error| format!("解析 {} 失败：{error}", path.display()))
}

fn write_profile(dir: &Path, profile: &AgentProfile) -> Result<(), String> {
    let path = dir.join(format!("{}.json", profile.id));
    let body = serde_json::to_string_pretty(profile)
        .map_err(|error| format!("序列化 Agent 失败：{error}"))?;
    // 原子写：先写临时文件再 rename，崩在中间不会留下半个 JSON。
    let temp = dir.join(format!(".{}.json.tmp", profile.id));
    std::fs::write(&temp, body)
        .map_err(|error| format!("写入 {} 失败：{error}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("保存 {} 失败：{error}", path.display())
    })
}

/// 合并内置与用户 profile（同 id 时 user 覆盖 builtin）。
///
/// 坏文件跳过并打一行 stderr：一个手改坏的 JSON 不该让整张 Agent 列表消失。
pub fn list_at(dir: &Path) -> Vec<AgentProfile> {
    let mut out = builtin();
    let mut extras: Vec<AgentProfile> = Vec::new();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();

    for path in paths {
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        match read_profile(&path) {
            Ok(mut profile) => {
                // 文件名是 id 的唯一真相：手改过 id 字段也不该造出两份。
                profile.id = stem.to_string();
                profile.origin = Origin::User;
                if let Some(slot) = out.iter_mut().find(|item| item.id == profile.id) {
                    profile.overrides_builtin = true;
                    *slot = profile;
                } else {
                    profile.overrides_builtin = false;
                    extras.push(profile);
                }
            }
            Err(error) => eprintln!("[prism] 跳过 Agent {stem}：{error}"),
        }
    }

    extras.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(extras);
    out
}

/// 全部 Agent（内置 + 自定义，含覆盖）。
pub fn list() -> Vec<AgentProfile> {
    match user_dir() {
        Some(dir) => list_at(&dir),
        None => builtin(),
    }
}

/// 保存（新建或覆盖）。同 id 的内置 preset 会被"覆盖"，而不是改名。
pub fn save_at(dir: &Path, profile: AgentProfile) -> Result<AgentProfile, String> {
    let mut profile = normalize(profile)?;
    profile.origin = Origin::User;
    profile.overrides_builtin = builtin().iter().any(|item| item.id == profile.id);
    write_profile(dir, &profile)?;
    Ok(profile)
}

pub fn save(profile: AgentProfile) -> Result<AgentProfile, String> {
    let dir = user_dir().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    save_at(&dir, profile)
}

/// 删除自定义 profile。删掉的若是内置覆盖，效果等于"还原内置"。
pub fn delete_at(dir: &Path, id: &str) -> Result<(), String> {
    let id = normalize_id(id)?;
    let path = dir.join(format!("{id}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if builtin().iter().any(|item| item.id == id) {
                Err(format!("「{id}」是内置 Agent，没有可删除的自定义覆盖（它本来就在）"))
            } else {
                Err(format!("没有找到 Agent「{id}」"))
            }
        }
        Err(error) => Err(format!("删除 {} 失败：{error}", path.display())),
    }
}

pub fn delete(id: &str) -> Result<(), String> {
    let dir = user_dir().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    delete_at(&dir, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prism-agents-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn custom(id: &str) -> AgentProfile {
        AgentProfile {
            id: id.to_string(),
            name: "审查员".to_string(),
            description: String::new(),
            origin: Origin::Builtin,
            overrides_builtin: false,
            executor: "native".to_string(),
            model: None,
            system_prompt: String::new(),
            permission: Permission::ReadOnly,
            workspace: Workspace::Shared,
            tools: ToolSelection::default(),
            mcp: Vec::new(),
            tags: Vec::new(),
            link_dirs: Vec::new(),
            gate_default: false,
        }
    }

    #[test]
    fn builtin_covers_the_documented_presets() {
        let ids: Vec<String> = builtin().into_iter().map(|profile| profile.id).collect();
        for expected in [
            "explore",
            "general",
            "frontend",
            "backend",
            "architect",
            "docs",
            "verifier",
            "dsh-research",
        ] {
            assert!(ids.contains(&expected.to_string()), "缺少内置 Agent：{expected}");
        }
    }

    /// `explore` / `general` 必须和 native 子代理的老枚举对得上，否则老会话
    /// 里的 `task` 调用会突然找不到对应 Agent。
    #[test]
    fn explore_and_general_match_subagent_defaults() {
        let all = builtin();
        let explore = all.iter().find(|item| item.id == "explore").unwrap();
        let general = all.iter().find(|item| item.id == "general").unwrap();
        assert_eq!(explore.permission, Permission::ReadOnly);
        assert_eq!(general.permission, Permission::WorkspaceWrite);
    }

    #[test]
    fn rejects_ids_that_could_escape_the_directory() {
        assert!(normalize_id("../etc/passwd").is_err());
        assert!(normalize_id("a/b").is_err());
        assert!(normalize_id("").is_err());
        assert!(normalize_id("x").is_err());
        assert!(normalize_id("-x").is_err());
        assert!(normalize_id("中文").is_err());
        assert_eq!(normalize_id("  Code-Reviewer  ").unwrap(), "code-reviewer");
    }

    #[test]
    fn deny_wins_over_allow() {
        let mut profile = custom("reviewer");
        profile.tools = ToolSelection {
            shared: Some(vec!["read_file".into(), "edit_file".into()]),
            private: vec!["edit_file".into()],
            deny: vec!["edit_file".into()],
        };
        let saved = normalize(profile).unwrap();
        assert_eq!(saved.tools.shared.unwrap(), vec!["read_file"]);
        assert!(saved.tools.private.is_empty());
    }

    #[test]
    fn user_file_overrides_builtin_and_delete_restores_it() {
        let dir = temp_dir("override");
        assert_eq!(list_at(&dir).len(), builtin().len());

        let mut profile = custom("explore");
        profile.name = "我的探索者".to_string();
        save_at(&dir, profile).unwrap();

        let merged = list_at(&dir);
        let explore = merged.iter().find(|item| item.id == "explore").unwrap();
        assert_eq!(explore.name, "我的探索者");
        assert_eq!(explore.origin, Origin::User);
        assert!(explore.overrides_builtin);
        // 覆盖不是新增：总数不变。
        assert_eq!(merged.len(), builtin().len());

        delete_at(&dir, "explore").unwrap();
        let restored = list_at(&dir);
        let explore = restored.iter().find(|item| item.id == "explore").unwrap();
        assert_eq!(explore.name, "探索者");
        assert_eq!(explore.origin, Origin::Builtin);
        assert!(!explore.overrides_builtin);
    }

    #[test]
    fn builtin_cannot_be_deleted_when_not_overridden() {
        let dir = temp_dir("delete-builtin");
        let error = delete_at(&dir, "verifier").unwrap_err();
        assert!(error.contains("内置"), "{error}");
        assert!(delete_at(&dir, "nope").unwrap_err().contains("没有找到"));
    }

    #[test]
    fn broken_file_is_skipped_without_losing_the_rest() {
        let dir = temp_dir("broken");
        save_at(&dir, custom("good")).unwrap();
        std::fs::write(dir.join("broken.json"), "{ not json").unwrap();

        let ids: Vec<String> = list_at(&dir).into_iter().map(|item| item.id).collect();
        assert!(ids.contains(&"good".to_string()));
        assert!(!ids.contains(&"broken".to_string()));
    }

    #[test]
    fn filename_is_the_source_of_truth_for_id() {
        let dir = temp_dir("filename");
        // 文件里写着别的 id，仍以文件名为准——否则一个文件能造出两份 Agent。
        std::fs::write(
            dir.join("real.json"),
            r#"{"id":"fake","name":"手改过","executor":"native"}"#,
        )
        .unwrap();
        let ids: Vec<String> = list_at(&dir).into_iter().map(|item| item.id).collect();
        assert!(ids.contains(&"real".to_string()));
        assert!(!ids.contains(&"fake".to_string()));
    }
}
