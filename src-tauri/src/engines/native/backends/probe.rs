//! 后端探测：装没装、版本够不够、路径在哪。
//!
//! 探测结果**不落盘成事实**：只缓存用户手动指定的路径（`~/.prism/toolchains/index.json`），
//! 版本每次进程内重探一次。机器上换了 Ghidra 版本，Prism 不能还按老版本解析。
//!
//! 查找顺序（命中即停）：
//! 1. 用户覆盖（设置页 / index.json）
//! 2. 环境变量（`PRISM_UPX` / `GHIDRA_HOME` …）
//! 3. `PATH`
//! 4. 常见安装位
//!
//! 不扫全盘：找不到就是找不到，报未接入，比扫 30 秒然后超时诚实。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::exec::{self, ExecSpec};

/// 一个外部后端的静态描述。
#[derive(Debug, Clone, Copy)]
pub struct Backend {
    pub id: &'static str,
    pub name: &'static str,
    /// 低于这个版本不接（`None` = 不比版本）。
    pub min_version: Option<&'static str>,
    /// 一句可复制的安装指引。
    pub install: &'static str,
    /// 受它影响的工具（设置页直接列）。
    pub tools: &'static [&'static str],
    /// 覆盖路径用的环境变量。
    pub env: &'static [&'static str],
}

pub const BACKENDS: &[Backend] = &[
    Backend {
        id: "upx",
        name: "UPX",
        min_version: Some("3.9"),
        install: "brew install upx",
        tools: &["bin.unpack.upx"],
        env: &["PRISM_UPX"],
    },
    Backend {
        id: "radare2",
        name: "radare2",
        min_version: Some("5.5"),
        install: "brew install radare2",
        tools: &["bin.radare2.scan"],
        env: &["PRISM_R2"],
    },
    Backend {
        id: "ghidra",
        name: "Ghidra",
        min_version: Some("11.0"),
        install: "brew install ghidra",
        tools: &["bin.ghidra.analyze", "bin.ghidra.decompile_func"],
        env: &["PRISM_GHIDRA_HOME", "GHIDRA_HOME"],
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendState {
    /// 还没探过（进程刚起）。
    Unknown,
    Ready,
    Missing,
    TooOld { found: String, need: String },
    /// 找到了但跑不起来（没执行权限 / 缺依赖）。
    Broken(String),
}

impl BackendState {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendState::Unknown => "unknown",
            BackendState::Ready => "ready",
            BackendState::Missing => "missing",
            BackendState::TooOld { .. } => "too-old",
            BackendState::Broken(_) => "broken",
        }
    }
}

/// 路径是从哪儿找到的（界面上解释"为什么是这一个"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeSource {
    Override,
    Env,
    Path,
    KnownDir,
    Unknown,
}

impl ProbeSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProbeSource::Override => "override",
            ProbeSource::Env => "env",
            ProbeSource::Path => "path",
            ProbeSource::KnownDir => "known-dir",
            ProbeSource::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackendStatus {
    pub id: &'static str,
    pub name: &'static str,
    pub state: BackendState,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub source: ProbeSource,
    pub note: Option<String>,
    pub install: &'static str,
    pub tools: &'static [&'static str],
}

impl BackendStatus {
    fn missing(def: &Backend) -> BackendStatus {
        BackendStatus {
            id: def.id,
            name: def.name,
            state: BackendState::Missing,
            path: None,
            version: None,
            source: ProbeSource::Unknown,
            note: None,
            install: def.install,
            tools: def.tools,
        }
    }
}

pub fn find(id: &str) -> Option<&'static Backend> {
    BACKENDS.iter().find(|def| def.id == id)
}

/// 可执行文件名（和 id 不一定同名：radare2 → r2）。
fn binary_name(id: &'static str) -> &'static str {
    match id {
        "radare2" => "r2",
        "dotnet" => "ilspycmd",
        // Ghidra 真正的入口是 `<home>/support/analyzeHeadless`：P2.3 在
        // `locate` 里特判目录形态，这里只给文件名。
        "ghidra" => "analyzeHeadless",
        other => other,
    }
}

fn version_args(id: &str) -> &'static [&'static str] {
    match id {
        "radare2" => &["-v"],
        // Ghidra 的版本写在 `<home>/Ghidra/application.properties` 里，
        // 不在命令输出里。P2.3 接它时改成读文件——不要拿 `--version` 打出来的
        // usage 去正则，那条路会读出乱七八糟的数字。
        "ghidra" => &[],
        _ => &["--version"],
    }
}

/// 探一个后端。**不缓存**，调用方决定要不要记。
pub fn probe(id: &str) -> BackendStatus {
    let Some(def) = find(id) else {
        // 不认识的 id 不编状态：报未接入。
        return BackendStatus {
            id: "unknown",
            name: "unknown",
            state: BackendState::Missing,
            path: None,
            version: None,
            source: ProbeSource::Unknown,
            note: Some(format!("没有登记过的后端：{id}")),
            install: "",
            tools: &[],
        };
    };
    let Some((path, source)) = locate(def) else {
        return BackendStatus::missing(def);
    };

    match read_version(def, &path) {
        Ok((version, note)) => {
            if let Some(need) = def.min_version {
                if !at_least(&version, need) {
                    return BackendStatus {
                        state: BackendState::TooOld {
                            found: version.clone(),
                            need: need.to_string(),
                        },
                        version: Some(version),
                        path: Some(path),
                        source,
                        note,
                        ..BackendStatus::missing(def)
                    };
                }
            }
            BackendStatus {
                id: def.id,
                name: def.name,
                state: BackendState::Ready,
                path: Some(path),
                version: Some(version),
                source,
                note,
                install: def.install,
                tools: def.tools,
            }
        }
        Err(why) => BackendStatus {
            state: BackendState::Broken(why),
            path: Some(path),
            source,
            ..BackendStatus::missing(def)
        },
    }
}

fn read_version(def: &Backend, program: &Path) -> Result<(String, Option<String>), String> {
    if def.id == "ghidra" {
        return ghidra_version(program);
    }
    let mut spec = ExecSpec::new(program)
        .args(version_args(def.id).iter().copied())
        // 冷启动可能撞上 Gatekeeper 扫描，或者赶上 cargo 全量重编译把 CPU
        // 吃满——3 秒会把明明能跑的 r2 误杀成 Broken。
        .timeout(Duration::from_secs(10));
    spec.stdout_cap = 8 * 1024;
    spec.stderr_cap = 8 * 1024;
    let out = exec::run(&spec).map_err(|error| error.to_string())?;
    if out.timed_out {
        return Err(format!(
            "起了进程但 {} 秒内没退出（机器可能正忙），输出：{}",
            spec.timeout.as_secs(),
            out.tail_text(120)
        ));
    }
    let text = format!("{}\n{}", out.stdout_text(), out.stderr_text());
    let version = version_token(&text).ok_or_else(|| {
        let tail = out.tail_text(120);
        format!("跑起来了但读不到版本号（输出：{tail}）")
    })?;
    Ok((version, None))
}

/// Ghidra 的版本在 `<home>/Ghidra/application.properties` 里，不在命令输出里。
///
/// `program` 是 `<home>/support/analyzeHeadless`，所以 home 是它的上两级。
/// 先解符号链接再往上找：用户手动指定的路径可能是个 symlink
/// （比如指到 Homebrew Cellar 里），拿 symlink 路径数两级会数错地方。
fn ghidra_version(program: &Path) -> Result<(String, Option<String>), String> {
    let program = std::fs::canonicalize(program).unwrap_or_else(|_| program.to_path_buf());
    let home = program
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("analyzeHeadless 路径不对：{}", program.display()))?;
    let properties = home.join("Ghidra/application.properties");
    let text = std::fs::read_to_string(&properties)
        .map_err(|error| format!("读不到 {}：{error}", properties.display()))?;
    let version = properties_version(&text).ok_or_else(|| {
        format!(
            "{} 里没有 application.version：这份安装可能不完整",
            properties.display()
        )
    })?;

    // java 缺失是硬故障（Ghidra 起不来）；版本偏低只提示，不拦。
    let major = java_major()?;
    if major < 17 {
        return Err(format!("java 主版本 {major} 太老（Ghidra 需要 JDK 17+）"));
    }
    let note = if at_least(&version, "11.2") && major < 21 {
        Some(format!(
            "Ghidra {version} 建议 JDK 21，本机 java 是 {major}；能跑，但大样本可能更慢。"
        ))
    } else {
        None
    };
    Ok((version, note))
}

/// 从 `application.properties` 里取版本号。
fn properties_version(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("application.version=")
            .map(|value| value.trim().trim_matches('"').to_string())
            .filter(|value| !value.is_empty())
    })
}

/// 本机 java 的位置：`JAVA_HOME` → `PATH` → Homebrew 的 keg-only openjdk。
///
/// 返回 (java 可执行文件, 推断出的 `JAVA_HOME`)。Homebrew 的 openjdk@*
/// 是 keg-only，`java` 不进 bin，analyzeHeadless 自己找不到——这两样
/// 得显式喂给 Ghidra；版本探测（[`java_major`]）与运行时（ghidra.rs 的
/// `JAVA_HOME` 透传）共用这一份，别处再造一份迟早对不上。
pub(crate) fn locate_java() -> Option<(PathBuf, Option<PathBuf>)> {
    if let Some(home) = std::env::var_os("JAVA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|home| is_jdk_home(home))
    {
        return Some((home.join("bin/java"), Some(home)));
    }
    // PATH 上的 java：能反推出 JAVA_HOME 才信。`/usr/bin/java` 这种 stub
    // 只是壳——机器上没注册 JVM 时它照样起不来，得让位给 Homebrew 的。
    let from_path = super::super::runtime::which("java")
        .map(|java| (java.clone(), java_home_of(&java)));
    if let Some(found @ (_, Some(_))) = &from_path {
        return Some(found.clone());
    }
    // Homebrew 的 openjdk@* / openjdk 装在 opt 下；多个就取版本号最大的那份。
    let mut candidates: Vec<PathBuf> = Vec::new();
    for base in [
        "/opt/homebrew/opt",
        "/usr/local/opt",
        "/home/linuxbrew/.linuxbrew/opt",
    ] {
        let Ok(entries) = std::fs::read_dir(base) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "openjdk" || name.starts_with("openjdk@") {
                let java = entry.path().join("bin/java");
                if is_executable(&java) {
                    candidates.push(java);
                }
            }
        }
    }
    candidates.sort();
    if let Some(java) = candidates.pop() {
        let home = java_home_of(&java);
        return Some((java, home));
    }
    from_path
}

/// 从 java 可执行路径反推 `JAVA_HOME`；推不出就 `None`——这时不传，
/// 让 Ghidra 自己顺着 PATH 找。
///
/// 光查 `bin/java` 可执行会自我兜圈：`/usr/bin/java` 这种 stub 没注册
/// JVM 时照样起不来，而它反推出的 home 是 `/usr`，`$JAVA_HOME/bin/java`
/// 又恰好是 stub 自己。真 JDK home 都带 `release` 标记，拿它做指纹。
fn java_home_of(java: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(java).unwrap_or_else(|_| java.to_path_buf());
    let home = real.parent()?.parent()?.to_path_buf();
    is_jdk_home(&home).then_some(home)
}

/// 像不像一个真正的 JDK home：有 `release` 标记、`bin/java` 可执行。
fn is_jdk_home(home: &Path) -> bool {
    home.join("release").is_file() && is_executable(&home.join("bin/java"))
}

/// 本机 java 的主版本号（`JAVA_HOME` → `PATH` → Homebrew keg-only）。
fn java_major() -> Result<u32, String> {
    let (java, _) = locate_java()
        .ok_or_else(|| "找不到 java（Ghidra 需要 JDK 17+，装好后点「重新检测」）".to_string())?;
    let out = exec::run(
        &ExecSpec::new(java)
            .arg("-version")
            .timeout(Duration::from_secs(10)),
    )
    .map_err(|error| format!("java 起不来：{error}"))?;
    let text = format!("{}\n{}", out.stdout_text(), out.stderr_text());
    let version = version_token(&text)
        .ok_or_else(|| format!("java -version 里读不到版本号（输出：{}）", out.tail_text(120)))?;
    version
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .ok_or_else(|| format!("java 版本号看不懂：{version}"))
}

fn locate(def: &Backend) -> Option<(PathBuf, ProbeSource)> {
    let binary = binary_name(def.id);

    // 1. 用户覆盖：可以指目录（Ghidra 指 home，其余指二进制所在目录），也可以指二进制本身。
    if let Some(raw) = overrides().get(def.id) {
        if let Some(candidate) = candidate_for(def, Path::new(raw)) {
            return Some((candidate, ProbeSource::Override));
        }
    }

    // 2. 环境变量。
    for key in def.env {
        if let Some(raw) = std::env::var_os(key).filter(|value| !value.is_empty()) {
            if let Some(candidate) = candidate_for(def, &PathBuf::from(raw)) {
                return Some((candidate, ProbeSource::Env));
            }
        }
    }

    // 3. 内置目录（`~/.prism/toolchains/<id>/`）：以后放 Prism 自带的工具链。
    if let Some(dir) = bundled_dir(def.id) {
        if let Some(candidate) = candidate_for(def, &dir) {
            return Some((candidate, ProbeSource::KnownDir));
        }
    }

    // 4. PATH。
    if let Some(found) = super::super::runtime::which(binary) {
        return Some((found, ProbeSource::Path));
    }

    // 5. 常见安装位。Ghidra 是 `/Applications/ghidra_*/support/analyzeHeadless`。
    if def.id == "ghidra" {
        for home in ghidra_homes() {
            if let Some(candidate) = candidate_for(def, &home) {
                return Some((candidate, ProbeSource::KnownDir));
            }
        }
    }
    for dir in known_dirs() {
        if let Some(candidate) = candidate_for(def, Path::new(dir)) {
            return Some((candidate, ProbeSource::KnownDir));
        }
    }
    None
}

/// 一个候选路径 → 真正可执行的入口。
///
/// Ghidra 的目录形态是 `<home>/support/analyzeHeadless`，其它后端是 `<dir>/<binary>`。
fn candidate_for(def: &Backend, path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        if def.id == "ghidra" {
            for relative in ["support/analyzeHeadless", "analyzeHeadless"] {
                let candidate = path.join(relative);
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
            return None;
        }
        let candidate = path.join(binary_name(def.id));
        return is_executable(&candidate).then_some(candidate);
    }
    is_executable(path).then(|| path.to_path_buf())
}

/// `~/.prism/toolchains/<id>/`（设计里留给"Prism 自带工具链"，现在通常为空）。
fn bundled_dir(id: &str) -> Option<PathBuf> {
    crate::engines::opencode::search::prism_dir().map(|dir| dir.join("toolchains").join(id))
}

/// `/Applications/ghidra_*`、`~/ghidra_*`，以及 Homebrew formula 的
/// `Cellar/ghidra/*/libexec`——formula 不装 `/Applications`，也不把
/// `analyzeHeadless` 链进 bin，不认这个位置探测永远是 Missing。
fn ghidra_homes() -> Vec<PathBuf> {
    let mut bases = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        bases.push(PathBuf::from(home));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for base in bases {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with("ghidra") && entry.path().is_dir() {
                out.push(entry.path());
            }
        }
    }
    for cellar in ["/opt/homebrew/Cellar", "/usr/local/Cellar"] {
        let Ok(entries) = std::fs::read_dir(Path::new(cellar).join("ghidra")) else {
            continue;
        };
        for entry in entries.flatten().filter(|entry| entry.path().is_dir()) {
            out.push(entry.path().join("libexec"));
        }
    }
    // 新版本排前面：同名命中时 candidate_for 取第一个。
    out.sort();
    out.reverse();
    out
}

fn known_dirs() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["/opt/homebrew/bin", "/usr/local/bin"]
    } else if cfg!(windows) {
        &[]
    } else {
        &["/usr/local/bin", "/usr/bin"]
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

// ---------------------------------------------------------------- 版本号

/// 从一段版本输出里抠出版本号。
///
/// 认的形状：`upx 4.2.4` / `radare2 5.9.8 0 @ darwin-arm-64` / `v1.2.3`。
/// 只取前几行——有的工具在版本行之后会打印一堆版权年份，那也是数字带点。
pub fn version_token(text: &str) -> Option<String> {
    for line in text.lines().take(3) {
        for token in line.split_whitespace() {
            // Java 的 `java -version` 把版本写在引号里：`openjdk version "21.0.1"`。
            let token = token.trim_start_matches('v').trim_matches('"');
            let digits: String = token
                .chars()
                .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
                .collect();
            if digits.contains('.')
                && digits.ends_with(|ch: char| ch.is_ascii_digit())
                && digits.chars().next().is_some_and(|ch| ch.is_ascii_digit())
            {
                return Some(digits);
            }
        }
    }
    None
}

/// `found >= need`（按点分数字比，缺的位当 0）。
pub fn at_least(found: &str, need: &str) -> bool {
    let parse = |raw: &str| -> Vec<u32> {
        raw.split('.')
            .map(|part| {
                part.chars()
                    .take_while(|ch| ch.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    };
    let a = parse(found);
    let b = parse(need);
    for index in 0..a.len().max(b.len()) {
        let left = *a.get(index).unwrap_or(&0);
        let right = *b.get(index).unwrap_or(&0);
        if left != right {
            return left > right;
        }
    }
    true
}

// ---------------------------------------------------------------- 覆盖路径

fn overrides_file() -> Option<PathBuf> {
    crate::engines::opencode::search::prism_dir().map(|dir| dir.join("toolchains/index.json"))
}

/// 读用户覆盖（只存路径，不存探测结果）。
pub fn overrides() -> HashMap<String, String> {
    let Some(path) = overrides_file() else {
        return HashMap::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    if let Some(map) = value.as_object() {
        for (id, entry) in map {
            if let Some(raw) = entry.get("path").and_then(|item| item.as_str()) {
                if !raw.trim().is_empty() {
                    out.insert(id.clone(), raw.trim().to_string());
                }
            }
        }
    }
    out
}

/// 写/清用户覆盖。`path` 为空 = 删除覆盖（回到自动探测）。
pub fn set_override(id: &str, path: Option<&str>) -> Result<(), String> {
    let file = overrides_file().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    let mut map = overrides();
    match path.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => {
            map.insert(id.to_string(), value.to_string());
        }
        None => {
            map.remove(id);
        }
    }
    let mut object = serde_json::Map::new();
    for (key, value) in map {
        object.insert(key, serde_json::json!({ "path": value }));
    }
    let body = serde_json::to_string_pretty(&serde_json::Value::Object(object))
        .map_err(|error| format!("序列化失败：{error}"))?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("建目录失败：{error}"))?;
    }
    // 原子写：临时文件 + rename。
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, body.as_bytes()).map_err(|error| format!("写临时文件失败：{error}"))?;
    std::fs::rename(&tmp, &file).map_err(|error| format!("替换配置失败：{error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机回归：装了的后端必须探到 Ready；没装的报 Missing，
    /// 绝不允许装好了还报 Broken / TooOld（那是探测的 bug，用户没法自救）。
    #[test]
    #[ignore = "真机回归：需要本机装了对应的工具链"]
    fn probes_the_real_machine() {
        eprintln!("locate_java = {:?}", locate_java());
        for def in BACKENDS {
            let status = probe(def.id);
            eprintln!("{:?}", status);
            if status.path.is_some() {
                assert_eq!(
                    status.state,
                    BackendState::Ready,
                    "{} 在 {:?} 被判成 {:?}",
                    def.id,
                    status.path,
                    status.state
                );
            }
        }
    }

    #[test]
    fn reads_version_from_upx_output() {
        let text = "upx 4.2.4\nUCL data compression library 1.03\nCopyright (C) 1996-2023 Markus Oberhumer\n";
        assert_eq!(version_token(text).as_deref(), Some("4.2.4"));
    }

    #[test]
    fn reads_version_from_radare2_output() {
        let text = "radare2 5.9.8 0 @ darwin-arm-64\n";
        assert_eq!(version_token(text).as_deref(), Some("5.9.8"));
    }

    #[test]
    fn reads_version_with_v_prefix() {
        assert_eq!(version_token("tool v1.2.3\n").as_deref(), Some("1.2.3"));
    }

    #[test]
    fn reads_quoted_java_version() {
        let text = "openjdk version \"21.0.1\" 2024-04-16\nOpenJDK Runtime Environment\n";
        assert_eq!(version_token(text).as_deref(), Some("21.0.1"));
    }

    #[test]
    fn ignores_text_without_version() {
        assert!(version_token("command not found\n").is_none());
        assert!(version_token("").is_none());
    }

    #[test]
    fn compares_versions_numerically_not_lexically() {
        assert!(at_least("4.2.4", "3.9"));
        assert!(at_least("5.10.0", "5.9"));
        assert!(!at_least("4.5.0", "5.5"));
        assert!(at_least("5.5", "5.5"));
        assert!(at_least("5.5.1", "5.5"));
    }

    #[test]
    fn reads_ghidra_version_from_properties() {
        let text = "# Ghidra\napplication.version=11.2.1\napplication.release.name=Ghidra\n";
        assert_eq!(properties_version(text).as_deref(), Some("11.2.1"));
        assert_eq!(
            properties_version("application.version=\"11.0\"\n").as_deref(),
            Some("11.0")
        );
        assert!(properties_version("no version here").is_none());
    }

    #[test]
    fn unknown_backend_is_missing_not_panic() {
        let status = probe("nope");
        assert_eq!(status.state, BackendState::Missing);
        assert!(status.note.is_some());
    }
}
