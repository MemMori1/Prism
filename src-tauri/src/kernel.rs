//! 插件内核：装配、依赖校验、可逆关停。
//!
//! 定位
//! ----
//! `Agent = Model + Harness`。模型那一侧的事（选哪个工具、要不要自造工具）
//! 不归这里管；内核只回答一个问题：**宿主此刻装了哪些能力**。
//!
//! 三条约束决定了它只能长成这样：
//!
//! 1. Rust 没有稳定 ABI，插件是编译进二进制的模块（`dlopen` 那套要和宿主锁死
//!    编译器与依赖版本，桌面端不值得）。所以"热"的是**装配**，不是代码。
//! 2. Tauri 的 `manage` 绑具体类型、且无法撤销。所以交给 Tauri 托管的服务
//!    没有 disposer——需要"卸干净"的副作用（线程、子进程、定时器）必须用
//!    [`KernelCtx::on_shutdown`] 显式登记。
//! 3. `generate_handler!` 是编译期宏，IPC 的可插拔要等第 3 步的 `kernel_invoke`。
//!
//! 这一步的收益不是"能热插拔了"，而是：装配从散在 `lib.rs` 里的一串
//! `app.manage(...)`，变成一张**有顺序、有依赖声明、可逆**的清单。

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::engines::engine::Engine;

/// 撤销一次副作用。
pub type Disposer = Box<dyn FnOnce() + Send>;

/// 一个可装卸的能力单元。
///
/// 刻意没有 `shutdown` 方法：需要收尾的副作用在 `setup` 里用
/// [`KernelCtx::on_shutdown`] 登记。这样"装了什么都说不清"的可能性被结构
/// 消灭了——每个副作用在建立它的一行代码旁边就写好了怎么拆。
pub trait Plugin: Send + Sync {
    /// 稳定标识，形如 `engine.opencode`。会出现在依赖声明和日志里。
    fn id(&self) -> &'static str;

    /// 依赖的其他插件。只做校验（缺失即装配失败），不参与重排：
    /// 装配顺序就是声明顺序，"谁先起"保持显式，不让排序算法替人做决定。
    fn requires(&self) -> &'static [&'static str] {
        &[]
    }

    /// 建立能力。可以有副作用，但每个副作用都该有对应的撤销登记。
    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String>;
}

/// 撤销栈：后登记的先执行。
///
/// 顺序是有意义的——依赖别人建立的东西，必须晚于被依赖者消失。否则卸载时
/// 会先拆掉底座，再去拆架在它上面的东西。
#[derive(Default)]
pub struct Disposers {
    entries: Vec<(&'static str, Disposer)>,
}

impl Disposers {
    pub fn push(&mut self, owner: &'static str, dispose: impl FnOnce() + Send + 'static) {
        self.entries.push((owner, Box::new(dispose)));
    }

    /// 逆序执行并清空。返回被拆掉的插件（装配顺序），便于日志。
    /// 可重入：第二次调用时栈已空，直接返回空列表。
    pub fn drain_reverse(&mut self) -> Vec<&'static str> {
        let mut torn_down = Vec::with_capacity(self.entries.len());
        while let Some((owner, dispose)) = self.entries.pop() {
            dispose();
            torn_down.push(owner);
        }
        torn_down
    }
}

/// 装配前的静态校验：重复 id、依赖缺失或声明顺序颠倒。
///
/// 纯函数，不碰 `AppHandle`——这样才能在没有 Tauri 运行时的情况下测它。
fn validate(plugins: &[Arc<dyn Plugin>]) -> Result<(), String> {
    let mut seen: Vec<&'static str> = Vec::with_capacity(plugins.len());
    for plugin in plugins {
        if seen.contains(&plugin.id()) {
            return Err(format!("插件 {} 声明了两次", plugin.id()));
        }
        for dependency in plugin.requires() {
            if !seen.contains(dependency) {
                return Err(format!(
                    "插件 {} 依赖 {}，但后者未装配或排在了它后面",
                    plugin.id(),
                    dependency
                ));
            }
        }
        seen.push(plugin.id());
    }
    Ok(())
}

/// 插件开关文件（`~/.prism/plugins.json`）。
const CONFIG_FILE: &str = "plugins.json";

/// 清单里一项：给工坊「插件」页看的。**声明**（清单里有它）与**结果**
/// （这次到底装没装）分开报，用户才知道"关掉一个插件"到底影响了什么。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub requires: Vec<String>,
    /// `active` / `disabled`（配置关掉）/ `cascaded`（依赖被关，级联关闭）。
    pub state: String,
    /// 非 active 时的原因，直接显示给用户。
    pub reason: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct PluginConfig {
    /// 关掉的插件 id。默认空＝全装。
    #[serde(default)]
    disabled: Vec<String>,
}

/// 读"哪些插件被关掉"。
///
/// 文件不存在、读不了、解析失败一律**全开**：配置坏掉不该让应用起不来，
/// 最坏也只是回到默认状态。这和"装配期失败要报错"并不矛盾——配置错是用户
/// 输入问题，插件依赖错是代码问题，两者的处置本来就该不同。
fn disabled_from_disk() -> Vec<String> {
    let Some(dir) = crate::engines::opencode::search::prism_dir() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(dir.join(CONFIG_FILE)) else {
        return Vec::new();
    };
    match serde_json::from_str::<PluginConfig>(&text) {
        Ok(config) => config.disabled,
        Err(error) => {
            eprintln!("[prism] {CONFIG_FILE} 解析失败，按全开处理：{error}");
            Vec::new()
        }
    }
}

/// 改一个插件的开关（写 `~/.prism/plugins.json`）。
///
/// **下次启动才生效**：装配只在 `setup` 里跑一次，运行期把插件装进来或拆出去
/// 需要各插件自己支持热插拔，而 Tauri 的 `manage` 根本不能撤销。与其假装能热切，
/// 不如把"要重启"写在返回值旁边的界面上。
///
/// 只动 `disabled` 一个键，其余字段原样保留（用户可能手写过别的配置）。
pub fn set_enabled(id: &str, enabled: bool) -> Result<(), String> {
    let dir = crate::engines::opencode::search::prism_dir()
        .ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    let path = dir.join(CONFIG_FILE);

    let mut root: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text).unwrap_or_else(|error| {
            // 配置坏了就重写一份：留着坏文件只会让"点了没反应"变成常态。
            eprintln!("[prism] {CONFIG_FILE} 解析失败，将重写：{error}");
            serde_json::json!({})
        }),
        _ => serde_json::json!({}),
    };
    if !root.is_object() {
        root = serde_json::json!({});
    }

    let mut disabled: Vec<String> = root
        .get("disabled")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    disabled.retain(|item| item != id);
    if !enabled {
        disabled.push(id.to_string());
    }
    root["disabled"] = serde_json::json!(disabled);

    let body = serde_json::to_string_pretty(&root)
        .map_err(|error| format!("序列化插件配置失败：{error}"))?;
    let temp = dir.join(format!(".{CONFIG_FILE}.tmp"));
    std::fs::write(&temp, body)
        .map_err(|error| format!("写入 {} 失败：{error}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("保存 {} 失败：{error}", path.display())
    })
}

/// 按开关求活动集：先剔掉被关掉的，再把"依赖已被剔掉"的级联剔掉。
///
/// 级联是必须的——否则只关掉 `engine.host` 会让 `routines` 在装配期直接失败，
/// 用户得到的不是"少个功能"，而是"应用起不来"。依赖没了就跟着不装，才符合
/// 一个可配置系统的直觉。
///
/// 返回（活动插件，被剔掉的原因）。原因是日志用的人类可读句子。
fn resolve(
    plugins: &[Arc<dyn Plugin>],
    disabled: &[String],
) -> (Vec<Arc<dyn Plugin>>, Vec<String>) {
    let mut active: Vec<Arc<dyn Plugin>> = Vec::with_capacity(plugins.len());
    let mut active_ids: Vec<&'static str> = Vec::with_capacity(plugins.len());
    let mut skipped: Vec<String> = Vec::new();

    for plugin in plugins {
        let id = plugin.id();
        let blocked = if disabled.iter().any(|off| off == id) {
            Some(format!("{id}：被配置关闭"))
        } else {
            plugin
                .requires()
                .iter()
                .find(|dependency| !active_ids.contains(dependency))
                .map(|dependency| format!("{id}：依赖的 {dependency} 未启用，级联关闭"))
        };

        match blocked {
            Some(reason) => skipped.push(reason),
            None => {
                active.push(plugin.clone());
                active_ids.push(id);
            }
        }
    }

    (active, skipped)
}

/// 装配期间递给插件的句柄。
pub struct KernelCtx<'a> {
    app: &'a AppHandle,
    id: &'static str,
    disposers: &'a mut Disposers,
    engines: &'a mut Vec<Arc<dyn Engine>>,
}

impl<'a> KernelCtx<'a> {
    /// 交给 Tauri 托管：既有的 `State<T>` 调用点全部靠这条路径继续工作。
    pub fn manage<T: Send + Sync + 'static>(&mut self, value: T) {
        self.app.manage(value);
    }

    /// 登记一个可撤销的副作用。
    pub fn on_shutdown(&mut self, dispose: impl FnOnce() + Send + 'static) {
        self.disposers.push(self.id, dispose);
    }

    /// 把一个引擎交给"引擎宿主"插件，由它在装配末尾统一收进注册表。
    ///
    /// 引擎不各自 `manage` 注册表，是因为注册表必须是**一份**：谁装了引擎
    /// 由活动集决定，而不是由哪个插件抢先 `new` 出来决定。
    pub fn register_engine(&mut self, engine: Arc<dyn Engine>) {
        self.engines.push(engine);
    }

    /// 目前已登记的引擎（按装配顺序）。
    pub fn engines(&self) -> &[Arc<dyn Engine>] {
        &self.engines
    }
}

/// 装配器。只在 `setup` 里活一次，装完就转成 [`KernelState`] 交给 Tauri。
pub struct Kernel {
    app: AppHandle,
    installed: Vec<&'static str>,
    disposers: Disposers,
    engines: Vec<Arc<dyn Engine>>,
    manifest: Vec<PluginInfo>,
}

impl Kernel {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            installed: Vec::new(),
            disposers: Disposers::default(),
            engines: Vec::new(),
            manifest: Vec::new(),
        }
    }

    /// 按配置装配：读 `~/.prism/plugins.json` 的 `disabled`，级联剔掉依赖方，
    /// 剩下的按声明顺序装上。
    pub fn install(&mut self, plugins: &[Arc<dyn Plugin>]) -> Result<(), String> {
        self.install_with(plugins, &disabled_from_disk())
    }

    /// 同上，但开关由调用方给定（测试用）。
    ///
    /// 活动集里的插件装配失败要**中止**并把原因抛出去——那是代码错，不该静默
    /// 降级成"半个应用"；被配置关掉则相反，是预期结果，只记日志。
    pub fn install_with(
        &mut self,
        plugins: &[Arc<dyn Plugin>],
        disabled: &[String],
    ) -> Result<(), String> {
        let (active, skipped) = resolve(plugins, disabled);
        validate(&active)?;

        // 清单在装配前就定格：即使某个插件 setup 失败，工坊里也能看到"它存在"，
        // 而不是整页空白——那会让人以为是没这个功能。
        self.manifest = plugins
            .iter()
            .map(|plugin| {
                let id = plugin.id();
                let requires = plugin
                    .requires()
                    .iter()
                    .map(|item| item.to_string())
                    .collect::<Vec<_>>();
                let blocked = if disabled.iter().any(|off| off == id) {
                    Some(("disabled", format!("{id}：被配置关闭")))
                } else if active.iter().any(|item| item.id() == id) {
                    None
                } else {
                    skipped
                        .iter()
                        .find(|reason| reason.starts_with(&format!("{id}：")))
                        .map(|reason| ("cascaded", reason.clone()))
                };
                match blocked {
                    None => PluginInfo {
                        id: id.to_string(),
                        requires,
                        state: "active".to_string(),
                        reason: None,
                    },
                    Some((state, reason)) => PluginInfo {
                        id: id.to_string(),
                        requires,
                        state: state.to_string(),
                        reason: Some(reason),
                    },
                }
            })
            .collect();

        for reason in &skipped {
            // 预期结果，不是错误：用 stderr 说明一句就够，不打断启动。
            eprintln!("[prism] 跳过插件：{reason}");
        }

        for plugin in &active {
            let mut ctx = KernelCtx {
                app: &self.app,
                id: plugin.id(),
                disposers: &mut self.disposers,
                engines: &mut self.engines,
            };
            plugin
                .setup(&mut ctx)
                .map_err(|error| format!("插件 {} 装配失败：{error}", plugin.id()))?;

            self.installed.push(plugin.id());
            #[cfg(debug_assertions)]
            println!("[prism] 已装配插件：{}", plugin.id());
        }
        Ok(())
    }

    /// 收尾：把撤销栈交给 Tauri 托管，退出时由 [`KernelState::shutdown`] 逆序执行。
    pub fn into_state(self) -> KernelState {
        KernelState {
            disposers: self.disposers,
            installed: self.installed,
            manifest: self.manifest,
        }
    }
}

/// 运行期的内核残影：只剩"怎么拆"与"装了哪些"。
///
/// 刻意不持有 `AppHandle`——托管状态里塞一个 AppHandle 会形成
/// `App -> State -> App` 的引用环，应用退出时永远释放不掉。
pub struct KernelState {
    disposers: Disposers,
    installed: Vec<&'static str>,
    manifest: Vec<PluginInfo>,
}

impl KernelState {
    /// 逆序执行撤销栈。可重入。
    pub fn shutdown(&mut self) {
        let torn_down = self.disposers.drain_reverse();
        #[cfg(debug_assertions)]
        if !torn_down.is_empty() {
            println!("[prism] 已关停 {} 个插件", torn_down.len());
        }
        self.installed.clear();
    }

    /// 本次装配的清单（含被关掉 / 级联关掉的项）。
    pub fn manifest(&self) -> Vec<PluginInfo> {
        self.manifest.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub {
        id: &'static str,
        requires: &'static [&'static str],
    }

    impl Plugin for Stub {
        fn id(&self) -> &'static str {
            self.id
        }

        fn requires(&self) -> &'static [&'static str] {
            self.requires
        }

        fn setup(&self, _ctx: &mut KernelCtx<'_>) -> Result<(), String> {
            Ok(())
        }
    }

    fn stubs(specs: &[(&'static str, &'static [&'static str])]) -> Vec<Arc<dyn Plugin>> {
        specs
            .iter()
            .map(|(id, requires)| {
                Arc::new(Stub {
                    id,
                    requires,
                }) as Arc<dyn Plugin>
            })
            .collect()
    }

    /// 依赖必须**先**装配。声明顺序即装配顺序，缺失就直接报错，
    /// 而不是"恰好因为排序靠前所以能跑"。
    #[test]
    fn missing_dependency_fails_loudly() {
        let error = validate(&stubs(&[("b", &["a"])])).unwrap_err();
        assert!(error.contains("依赖 a"), "{error}");
    }

    /// 依赖排在自己后面也算缺失——这是最容易写错的一种。
    #[test]
    fn dependency_declared_after_dependent_is_rejected() {
        let error = validate(&stubs(&[("b", &["a"]), ("a", &[])])).unwrap_err();
        assert!(error.contains("排在了它后面"), "{error}");
    }

    #[test]
    fn duplicate_id_is_rejected() {
        let error = validate(&stubs(&[("a", &[]), ("a", &[])])).unwrap_err();
        assert!(error.contains("两次"), "{error}");
    }

    #[test]
    fn satisfied_dependencies_pass() {
        assert!(validate(&stubs(&[("a", &[]), ("b", &["a"]), ("c", &["a", "b"])])).is_ok());
    }

    /// 关掉一个插件：它自己出列，其余不动。
    #[test]
    fn disabled_plugin_is_skipped() {
        let plugins = stubs(&[("a", &[]), ("b", &["a"])]);
        let (active, skipped) = resolve(&plugins, &["b".to_string()]);

        let ids: Vec<&str> = active.iter().map(|plugin| plugin.id()).collect();
        assert_eq!(ids, vec!["a"]);
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("被配置关闭"), "{:?}", skipped);
    }

    /// 依赖被关掉时，依赖方级联出列——而不是让装配期报错、应用起不来。
    #[test]
    fn dependents_of_disabled_plugin_cascade_out() {
        let plugins = stubs(&[("a", &[]), ("b", &["a"]), ("c", &[])]);
        let (active, skipped) = resolve(&plugins, &["a".to_string()]);

        let ids: Vec<&str> = active.iter().map(|plugin| plugin.id()).collect();
        assert_eq!(ids, vec!["c"]);
        assert_eq!(skipped.len(), 2);
        assert!(skipped.iter().any(|reason| reason.contains("级联关闭")), "{skipped:?}");

        // 级联之后的集合依然能过校验（不该再抛"依赖未装配"）。
        assert!(validate(&active).is_ok());
    }

    /// 级联是传递的：关掉根，整条链都下去。
    #[test]
    fn cascade_is_transitive() {
        let plugins = stubs(&[("a", &[]), ("b", &["a"]), ("c", &["b"])]);
        let (active, skipped) = resolve(&plugins, &["a".to_string()]);
        assert!(active.is_empty(), "{:?}", active.iter().map(|p| p.id()).collect::<Vec<_>>());
        assert_eq!(skipped.len(), 3);
    }

    /// 配置里写了不存在的 id：当作没写，不该影响装配。
    #[test]
    fn unknown_id_in_config_is_harmless() {
        let plugins = stubs(&[("a", &[]), ("b", &["a"])]);
        let (active, skipped) = resolve(&plugins, &["nope".to_string()]);
        assert_eq!(active.len(), 2);
        assert!(skipped.is_empty());
    }

    /// 撤销必须逆序：后建立的先拆，否则会先拆掉底座。
    #[test]
    fn disposers_run_in_reverse_order() {
        let mut stack = Disposers::default();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));

        for name in ["a", "b", "c"] {
            let log = log.clone();
            stack.push(name, move || log.lock().unwrap().push(name));
        }

        let order = stack.drain_reverse();
        assert_eq!(order, vec!["c", "b", "a"]);
        assert_eq!(log.lock().unwrap().as_slice(), &["c", "b", "a"]);

        // 可重入：再关一次不该重复执行。
        assert!(stack.drain_reverse().is_empty());
        assert_eq!(log.lock().unwrap().len(), 3);
    }
}
