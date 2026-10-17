//! 工坊：引擎资产发现（技能 / 命令 / 子代理 / 记忆 / 插件）。
//!
//! 发现逻辑一律做成"接受根路径的纯函数"：命令层只装配家目录与项目列表，
//! 单测注入 fixture 目录。全程只读，不写引擎任何目录。
//!
//! 各引擎资产口径（v2 官方文档 / 本机实测，2026-10）：
//! - OpenCode：`~/.config/opencode/{skills,agents,commands}`（含遗留单数 `command`）
//!   + `<项目>/.opencode/{...}`；技能兼容 `.claude/skills`（归 Claude 侧展示，避免重复）。
//! - Codex：`~/.codex/skills/<名>/SKILL.md`（`.system` 为内置）+ 插件缓存
//!   `~/.codex/plugins/cache/<市场>/<插件>/<版本>/`（版本目录含 `.codex-plugin/plugin.json`）。
//! - Claude Code：`~/.claude/{skills,commands,agents}` + `<项目>/.claude/{...}`。
//! - 记忆：`AGENTS.md`（Codex / OpenCode）与 `CLAUDE.md`（Claude），用户级与项目级。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::engines::time::file_modified_ms;

pub const KIND_SKILL: &str = "skill";
pub const KIND_COMMAND: &str = "command";
pub const KIND_AGENT: &str = "agent";
pub const KIND_MEMORY: &str = "memory";
pub const KIND_PLUGIN: &str = "plugin";

/// 资产条目（界面：左侧按 引擎×种类 分组，中部列表，右侧详情）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetItem {
    pub engine: String,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    /// 入口文件路径（技能给 SKILL.md；插件给 plugin.json，缺省给版本目录）。
    pub path: String,
    /// 归属：user / builtin / project。
    pub scope: String,
    /// 项目工作目录（scope = project 时）。
    pub project: Option<String>,
    pub updated_at: i64,
}

/// 资产入口文件内容（右侧预览）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetFile {
    pub path: String,
    pub content: String,
    pub truncated: bool,
}

/// 命令层装配的引擎根集合。
pub struct AssetRoots {
    pub opencode_config: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub claude_dir: Option<PathBuf>,
}

/// 入口：扫描三引擎的用户级与项目级资产（只读）。
pub fn collect(roots: &AssetRoots, project_cwds: &[String]) -> Vec<AssetItem> {
    let mut items = Vec::new();
    if let Some(config) = roots.opencode_config.as_deref() {
        collect_opencode(config, &mut items);
    }
    if let Some(codex) = roots.codex_home.as_deref() {
        collect_codex(codex, &mut items);
    }
    if let Some(claude) = roots.claude_dir.as_deref() {
        collect_claude(claude, &mut items);
    }
    for cwd in project_cwds {
        collect_project(Path::new(cwd), cwd, &mut items);
    }
    items.sort_by(|a, b| {
        (
            engine_rank(&a.engine),
            kind_rank(&a.kind),
            scope_rank(&a.scope),
            a.name.as_str(),
            a.path.as_str(),
        )
            .cmp(&(
                engine_rank(&b.engine),
                kind_rank(&b.kind),
                scope_rank(&b.scope),
                b.name.as_str(),
                b.path.as_str(),
            ))
    });
    items
}

fn engine_rank(engine: &str) -> u8 {
    match engine {
        "opencode" => 0,
        "codex" => 1,
        "claude" => 2,
        _ => 9,
    }
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        KIND_SKILL => 0,
        KIND_COMMAND => 1,
        KIND_AGENT => 2,
        KIND_MEMORY => 3,
        KIND_PLUGIN => 4,
        _ => 9,
    }
}

fn scope_rank(scope: &str) -> u8 {
    match scope {
        "user" => 0,
        "builtin" => 1,
        "project" => 2,
        _ => 9,
    }
}

// ---------------------------------------------------------------- 各引擎装配

fn collect_opencode(config: &Path, items: &mut Vec<AssetItem>) {
    items.extend(scan_skills(&config.join("skills"), "opencode", "user", None));
    items.extend(scan_markdown_tree(
        &config.join("agents"),
        "opencode",
        KIND_AGENT,
        "user",
        None,
    ));
    items.extend(scan_markdown_tree(
        &config.join("commands"),
        "opencode",
        KIND_COMMAND,
        "user",
        None,
    ));
    // v2 文档明示：遗留单数 `command/` 目录同样会被发现。
    items.extend(scan_markdown_tree(
        &config.join("command"),
        "opencode",
        KIND_COMMAND,
        "user",
        None,
    ));
    items.extend(file_asset(
        &config.join("AGENTS.md"),
        "opencode",
        KIND_MEMORY,
        "user",
        None,
    ));
}

fn collect_codex(home: &Path, items: &mut Vec<AssetItem>) {
    let skills = home.join("skills");
    // 用户技能（点目录跳过）；`.system` 单独作内置技能。
    items.extend(scan_skills(&skills, "codex", "user", None));
    items.extend(scan_skills(
        &skills.join(".system"),
        "codex",
        "builtin",
        None,
    ));
    items.extend(scan_codex_plugins(&home.join("plugins").join("cache")));
    items.extend(file_asset(
        &home.join("AGENTS.md"),
        "codex",
        KIND_MEMORY,
        "user",
        None,
    ));
    items.extend(scan_markdown_tree(
        &home.join("memories"),
        "codex",
        KIND_MEMORY,
        "user",
        None,
    ));
}

fn collect_claude(dir: &Path, items: &mut Vec<AssetItem>) {
    items.extend(scan_skills(&dir.join("skills"), "claude", "user", None));
    items.extend(scan_markdown_tree(
        &dir.join("commands"),
        "claude",
        KIND_COMMAND,
        "user",
        None,
    ));
    items.extend(scan_markdown_tree(
        &dir.join("agents"),
        "claude",
        KIND_AGENT,
        "user",
        None,
    ));
    items.extend(file_asset(
        &dir.join("CLAUDE.md"),
        "claude",
        KIND_MEMORY,
        "user",
        None,
    ));
}

fn collect_project(root: &Path, cwd: &str, items: &mut Vec<AssetItem>) {
    if !root.is_dir() {
        return;
    }
    let opencode = root.join(".opencode");
    items.extend(scan_skills(
        &opencode.join("skills"),
        "opencode",
        "project",
        Some(cwd),
    ));
    items.extend(scan_markdown_tree(
        &opencode.join("agents"),
        "opencode",
        KIND_AGENT,
        "project",
        Some(cwd),
    ));
    for commands in ["commands", "command"] {
        items.extend(scan_markdown_tree(
            &opencode.join(commands),
            "opencode",
            KIND_COMMAND,
            "project",
            Some(cwd),
        ));
    }
    // AGENTS.md 同被 Codex / OpenCode 读取，各自成行（引擎视角如实呈现）。
    items.extend(file_asset(
        &root.join("AGENTS.md"),
        "opencode",
        KIND_MEMORY,
        "project",
        Some(cwd),
    ));
    items.extend(file_asset(
        &root.join("AGENTS.md"),
        "codex",
        KIND_MEMORY,
        "project",
        Some(cwd),
    ));
    let claude = root.join(".claude");
    items.extend(scan_skills(
        &claude.join("skills"),
        "claude",
        "project",
        Some(cwd),
    ));
    items.extend(scan_markdown_tree(
        &claude.join("commands"),
        "claude",
        KIND_COMMAND,
        "project",
        Some(cwd),
    ));
    items.extend(scan_markdown_tree(
        &claude.join("agents"),
        "claude",
        KIND_AGENT,
        "project",
        Some(cwd),
    ));
    items.extend(file_asset(
        &root.join("CLAUDE.md"),
        "claude",
        KIND_MEMORY,
        "project",
        Some(cwd),
    ));
}

// ---------------------------------------------------------------- 扫描器

/// 技能目录：`<root>/*.md` 平铺 + `<root>/<名>/SKILL.md`（两层；
/// 官方允许更深，实际布局两层足够，后续按需加深度）。
fn scan_skills(root: &Path, engine: &str, scope: &str, project: Option<&str>) -> Vec<AssetItem> {
    let mut items = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return items;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();
        if file_name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_file() {
            if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                items.push(make_asset(engine, KIND_SKILL, &path, scope, project, None));
            }
        } else if file_type.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.is_file() {
                items.push(make_asset(
                    engine,
                    KIND_SKILL,
                    &skill_md,
                    scope,
                    project,
                    Some(&file_name),
                ));
            }
        }
    }
    items
}

/// Markdown 资产树（命令 / 子代理 / 记忆目录）：`<root>/**/*.md`，
/// 嵌套路径并入名称（如 `team/review`）；点目录与点文件跳过。
fn scan_markdown_tree(
    root: &Path,
    engine: &str,
    kind: &str,
    scope: &str,
    project: Option<&str>,
) -> Vec<AssetItem> {
    let mut items = Vec::new();
    collect_markdown(root, Path::new(""), 4, &mut |path, relative| {
        let raw = relative.to_string_lossy().to_string();
        let name = raw.trim_end_matches(".md").replace('\\', "/");
        items.push(make_asset(engine, kind, path, scope, project, Some(&name)));
    });
    items
}

fn collect_markdown(
    dir: &Path,
    prefix: &Path,
    depth: u8,
    visit: &mut impl FnMut(&Path, &Path),
) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut sorted: Vec<_> = entries.flatten().collect();
    sorted.sort_by_key(|entry| entry.file_name());
    for entry in sorted {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if file_name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        let relative = prefix.join(&file_name);
        if file_type.is_file() {
            if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                visit(&path, &relative);
            }
        } else if file_type.is_dir() {
            collect_markdown(&path, &relative, depth - 1, visit);
        }
    }
}

/// Codex 插件缓存：`<cache>/<市场>/<插件>/<版本>/` → 名称 `插件@市场`。
/// 入口取 `.codex-plugin/plugin.json`（缺失退版本目录）；描述优先取 manifest。
fn scan_codex_plugins(cache: &Path) -> Vec<AssetItem> {
    let mut items = Vec::new();
    for market_dir in read_dirs(cache) {
        let market = name_of(&market_dir);
        for plugin_dir in read_dirs(&market_dir) {
            let plugin = name_of(&plugin_dir);
            let Some(version_dir) = newest_dir(&plugin_dir) else {
                continue;
            };
            let manifest = version_dir.join(".codex-plugin").join("plugin.json");
            let entry = if manifest.is_file() {
                manifest
            } else {
                version_dir.clone()
            };
            let description = fs::read_to_string(&entry)
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|value| {
                    value
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|text| compact(text, 200))
                });
            items.push(AssetItem {
                engine: "codex".to_string(),
                kind: KIND_PLUGIN.to_string(),
                name: format!("{plugin}@{market}"),
                description,
                path: entry.display().to_string(),
                scope: "user".to_string(),
                project: None,
                updated_at: file_modified_ms(&entry),
            });
        }
    }
    items
}

/// 单文件资产（记忆 / 指令文件）：存在才收录。
fn file_asset(
    path: &Path,
    engine: &str,
    kind: &str,
    scope: &str,
    project: Option<&str>,
) -> Option<AssetItem> {
    if !path.is_file() {
        return None;
    }
    Some(make_asset(engine, kind, path, scope, project, None))
}

/// 组装一条资产：读取头部元数据；名称优先 frontmatter，其次调用方给的兜底名。
fn make_asset(
    engine: &str,
    kind: &str,
    path: &Path,
    scope: &str,
    project: Option<&str>,
    fallback_name: Option<&str>,
) -> AssetItem {
    let (meta_name, description) = read_meta(path);
    let name = meta_name
        .or_else(|| fallback_name.map(str::to_string))
        .unwrap_or_else(|| name_of(path));
    AssetItem {
        engine: engine.to_string(),
        kind: kind.to_string(),
        name,
        description,
        path: path.display().to_string(),
        scope: scope.to_string(),
        project: project.map(str::to_string),
        updated_at: file_modified_ms(path),
    }
}

/// 读取文件头部轻量元数据：frontmatter 的 `name` / `description`；
/// description 缺省时取正文首个非空行（截 120 字符）。
fn read_meta(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(text) = fs::read_to_string(path) else {
        return (None, None);
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut body_index = 0usize;

    if lines.first().map(|line| line.trim()) == Some("---") {
        let mut index = 1usize;
        while index < lines.len() {
            let trimmed = lines[index].trim();
            if trimmed == "---" {
                body_index = index + 1;
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("name:") {
                if name.is_none() {
                    name = clean_scalar(rest);
                }
            } else if let Some(rest) = trimmed.strip_prefix("description:") {
                if description.is_none() {
                    description = read_scalar(rest, &lines, &mut index);
                }
            }
            index += 1;
        }
    }

    if description.is_none() {
        for line in text.lines().skip(body_index) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            description = Some(compact(trimmed, 120));
            break;
        }
    }
    (name, description)
}

/// frontmatter 标量读取：普通单行直接清洗；`>` / `|` 块标记则拼接后续缩进行。
/// 返回时把 `index` 推进到块的最后一行（外层循环再 `+1`）。
fn read_scalar(first: &str, lines: &[&str], index: &mut usize) -> Option<String> {
    let value = first.trim();
    if !(value.is_empty() || value.starts_with('>') || value.starts_with('|')) {
        return clean_scalar(value);
    }
    let mut parts: Vec<String> = Vec::new();
    let mut cursor = *index + 1;
    while cursor < lines.len() {
        let line = lines[cursor];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            cursor += 1;
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            break;
        }
        parts.push(trimmed.to_string());
        cursor += 1;
    }
    if parts.is_empty() {
        return None;
    }
    *index = cursor - 1;
    Some(compact(&parts.join(" "), 200))
}

/// frontmatter 标量清洗：去引号；多行块标记（`>` / `|`）视同缺省。
fn clean_scalar(raw: &str) -> Option<String> {
    let value = raw.trim().trim_matches(|ch| ch == '"' || ch == '\'').trim();
    if value.is_empty() || value == ">" || value == "|" || value == ">-" || value == "|-" {
        return None;
    }
    Some(compact(value, 200))
}

/// 压平空白并截断。
fn compact(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= limit {
        flat
    } else {
        format!("{}…", chars[..limit].iter().collect::<String>())
    }
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn newest_dir(dir: &Path) -> Option<PathBuf> {
    read_dirs(dir)
        .into_iter()
        .max_by_key(|path| file_modified_ms(path))
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 读文本资产（预览）：字符上限截断；二进制字节用替换字符兜底。
pub fn read_text(path: &Path, limit: usize) -> Result<(String, bool), String> {
    /// 预览读入的字节硬上限（防误读大文件）。
    const MAX_BYTES: usize = 4 * 1024 * 1024;
    let bytes = fs::read(path).map_err(|cause| format!("读取失败：{cause}"))?;
    let (slice, byte_truncated) = if bytes.len() > MAX_BYTES {
        (&bytes[..MAX_BYTES], true)
    } else {
        (&bytes[..], false)
    };
    let text = String::from_utf8_lossy(slice).to_string();
    let chars: Vec<char> = text.chars().collect();
    let truncated = byte_truncated || chars.len() > limit;
    let content = if chars.len() > limit {
        chars[..limit].iter().collect()
    } else {
        text
    };
    Ok((content, truncated))
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("父目录")).expect("建目录");
        fs::write(path, content).expect("写文件");
    }

    /// 三个引擎根 + 一个项目目录的 fixture。
    fn fixture() -> (PathBuf, AssetRoots, PathBuf) {
        let base = std::env::temp_dir().join(format!("prism-workshop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let opencode = base.join("opencode");
        let codex = base.join("codex");
        let claude = base.join("claude");
        let project = base.join("proj");

        write(
            &opencode.join("skills/flat.md"),
            "---\nname: Flat Skill\ndescription: 平铺技能\n---\nbody\n",
        );
        write(
            &opencode.join("skills/dir-skill/SKILL.md"),
            "---\ndescription: >\n  目录技能的第一行\n  与第二行\n---\nbody\n",
        );
        write(
            &opencode.join("agents/reviewer.md"),
            "---\ndescription: 审查者\n---\npro\n",
        );
        write(&opencode.join("commands/team/review.md"), "Review $ARGUMENTS.\n");
        write(&opencode.join("AGENTS.md"), "# 全局指令\n");

        write(
            &codex.join("skills/figma/SKILL.md"),
            "---\nname: figma\ndescription: Figma MCP\n---\nbody\n",
        );
        write(
            &codex.join("skills/.system/skill-creator/SKILL.md"),
            "---\ndescription: 创建技能\n---\nbody\n",
        );
        write(
            &codex.join("plugins/cache/openai-bundled/browser/1.2.3/.codex-plugin/plugin.json"),
            "{\"name\":\"browser\",\"description\":\"浏览器插件\"}",
        );
        write(&codex.join("AGENTS.md"), "");

        write(
            &claude.join("skills/x/SKILL.md"),
            "---\ndescription: claude 技能\n---\nbody\n",
        );
        write(&claude.join("commands/c.md"), "hello\n");
        write(&claude.join("CLAUDE.md"), "记忆\n");

        write(
            &project.join(".opencode/skills/p/SKILL.md"),
            "---\ndescription: 项目技能\n---\nbody\n",
        );
        write(
            &project.join(".claude/skills/q/SKILL.md"),
            "---\ndescription: claude 项目技能\n---\nbody\n",
        );
        write(&project.join("AGENTS.md"), "项目指令\n");
        write(&project.join("CLAUDE.md"), "项目记忆\n");

        let roots = AssetRoots {
            opencode_config: Some(opencode),
            codex_home: Some(codex),
            claude_dir: Some(claude),
        };
        (base, roots, project)
    }

    #[test]
    fn collects_assets_across_engines() {
        let (base, roots, project) = fixture();
        let items = collect(&roots, &[project.display().to_string()]);

        let skills: Vec<&AssetItem> = items.iter().filter(|i| i.kind == KIND_SKILL).collect();
        // opencode 用户 2 + codex 用户 1 + codex 内置 1 + claude 用户 1 + 项目 opencode 1 + 项目 claude 1
        assert_eq!(skills.len(), 7, "技能数：{skills:#?}");
        assert!(skills
            .iter()
            .any(|i| i.engine == "opencode" && i.name == "Flat Skill" && i.scope == "user"));
        assert!(skills.iter().any(|i| i.engine == "opencode"
            && i.name == "dir-skill"
            && i.description.as_deref() == Some("目录技能的第一行 与第二行")));
        assert!(skills
            .iter()
            .any(|i| i.engine == "codex" && i.scope == "builtin" && i.name == "skill-creator"));
        assert!(skills
            .iter()
            .any(|i| i.engine == "claude" && i.project.is_some() && i.name == "q"));

        let commands: Vec<&AssetItem> =
            items.iter().filter(|i| i.kind == KIND_COMMAND).collect();
        assert_eq!(commands.len(), 2);
        assert!(commands
            .iter()
            .any(|i| i.name == "team/review" && i.engine == "opencode"));
        assert!(commands.iter().any(|i| i.engine == "claude" && i.name == "c"));

        let plugins: Vec<&AssetItem> = items.iter().filter(|i| i.kind == KIND_PLUGIN).collect();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "browser@openai-bundled");
        assert_eq!(plugins[0].description.as_deref(), Some("浏览器插件"));
        assert!(plugins[0].path.ends_with("plugin.json"));

        let memories: Vec<&AssetItem> = items.iter().filter(|i| i.kind == KIND_MEMORY).collect();
        // opencode 用户+项目、codex 用户+项目、claude 用户+项目 = 6
        assert_eq!(memories.len(), 6, "记忆数：{memories:#?}");
        assert!(memories.iter().any(|i| i.engine == "opencode" && i.project.is_some()));
        assert!(memories.iter().any(|i| i.engine == "codex" && i.project.is_some()));

        // 排序：引擎序 opencode → codex → claude（同引擎内种类序）
        let ranks: Vec<u8> = items.iter().map(|i| engine_rank(&i.engine)).collect();
        assert!(ranks.windows(2).all(|pair| pair[0] <= pair[1]), "引擎序：{ranks:?}");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_roots_yield_empty() {
        let roots = AssetRoots {
            opencode_config: Some(PathBuf::from("/nonexistent/opencode")),
            codex_home: None,
            claude_dir: None,
        };
        assert!(collect(&roots, &[]).is_empty());
    }

    #[test]
    fn reads_text_with_limit() {
        let base = std::env::temp_dir().join(format!("prism-workshop-read-{}", std::process::id()));
        let path = base.join("a.md");
        write(&path, "你好，世界");
        let (content, truncated) = read_text(&path, 3).expect("读取");
        assert_eq!(content, "你好，");
        assert!(truncated);
        let (all, truncated) = read_text(&path, 100).expect("读取");
        assert_eq!(all, "你好，世界");
        assert!(!truncated);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn frontmatter_scalar_edge_cases() {
        assert_eq!(clean_scalar(" \"带引号\" ").as_deref(), Some("带引号"));
        assert_eq!(clean_scalar(">"), None);
        assert_eq!(clean_scalar("  \n"), None);
    }

    /// 依赖本机真实引擎目录：`cargo test -- --ignored --nocapture`。
    #[test]
    #[ignore]
    fn lists_real_assets() {
        let roots = AssetRoots {
            opencode_config: crate::engines::opencode::locator::config_dir(),
            codex_home: crate::engines::codex::locator::codex_home(),
            claude_dir: crate::engines::claude::locator::config_root(),
        };
        let cwds = crate::ipc::known_project_cwds();
        let items = collect(&roots, &cwds);
        println!("工坊资产 {} 条（项目 {} 个）", items.len(), cwds.len());
        for item in &items {
            println!(
                "  [{} / {} / {}] {} — {}",
                item.engine,
                item.kind,
                item.scope,
                item.name,
                item.description.clone().unwrap_or_default()
            );
        }
    }
}
