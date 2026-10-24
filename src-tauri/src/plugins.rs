//! 宿主能力清单：prism 现在装了什么，全在这个文件里看得见。
//!
//! 每个 `Plugin` 只做三件事：建立自己的状态、把它挂到 Tauri（`manage`）或
//! 引擎注册表（`register_engine`）、把需要收尾的副作用登记成 disposer。
//!
//! 加一个能力 = 在这里加一个 struct + 在 `lib.rs` 的清单里加一行。不再需要
//! 去 `lib.rs` 的 `setup` 里翻一串 `app.manage(...)`，也不用担心漏了关停。

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::AppHandle;

use crate::engines::claude::engine::ClaudeEngine;
use crate::engines::codex::runtime::Runtime as CodexRuntime;
use crate::engines::dsh::runtime::Runtime as DshRuntime;
use crate::engines::engine::EngineRegistry;
use crate::engines::native::engine::NativeEngine;
use crate::engines::native::tools::{
    self as tools, BoxedRun, Permission, ToolDef, ToolImpl, ToolOrigin, ToolRegistry, ToolResult,
    ToolSchema,
};
use crate::engines::opencode::runtime::Runtime as OpencodeRuntime;
use crate::engines::quota::QuotaState;
use crate::kernel::{KernelCtx, Plugin};
use crate::mcp_client::{self, ServerSpec, StdioClient};
use crate::phone_link;
use crate::routines::scheduler::{self, RoutinesState};
use crate::tasks::TasksState;

/// OpenCode 引擎：拉起 `opencode serve` 子进程，走 HTTP/SSE。
pub struct OpencodeEngine {
    app: AppHandle,
}

impl OpencodeEngine {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Plugin for OpencodeEngine {
    fn id(&self) -> &'static str {
        "engine.opencode"
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        let runtime = OpencodeRuntime::new(self.app.clone());
        // 双写：注册表面向 trait 分发，Tauri 托管服务既有的 `State<Runtime>`
        // 调用点。`Runtime` 内部是 `Arc<Inner>`，两条路径看到的是同一个实例，
        // 不会各自拉起一个 serve 子进程。
        ctx.manage(runtime.clone());
        ctx.register_engine(Arc::new(runtime));
        Ok(())
    }
}

/// Codex 引擎：拉起 `codex app-server`，走 JSON-RPC。
pub struct CodexEngine {
    app: AppHandle,
}

impl CodexEngine {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Plugin for CodexEngine {
    fn id(&self) -> &'static str {
        "engine.codex"
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        let runtime = CodexRuntime::new(self.app.clone());
        ctx.manage(runtime.clone());
        ctx.register_engine(Arc::new(runtime));
        Ok(())
    }
}

/// 原生引擎：不拉进程，自己跑 turn 循环与工具。
pub struct NativeEnginePlugin;

impl Plugin for NativeEnginePlugin {
    fn id(&self) -> &'static str {
        "engine.native"
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        ctx.register_engine(Arc::new(NativeEngine::new()));
        Ok(())
    }
}

/// Claude 引擎：目前只读历史（运行轴后置），因此不 `manage` 任何状态。
pub struct ClaudeEnginePlugin;

impl Plugin for ClaudeEnginePlugin {
    fn id(&self) -> &'static str {
        "engine.claude"
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        ctx.register_engine(Arc::new(ClaudeEngine::new()));
        Ok(())
    }
}

/// DeepSeek Harness 引擎：拉起 `dsh --profile acp`，走 ACP v1（stdio JSON-RPC）。
pub struct DshEngine {
    app: AppHandle,
}

impl DshEngine {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Plugin for DshEngine {
    fn id(&self) -> &'static str {
        "engine.dsh"
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        let runtime = DshRuntime::new(Some(self.app.clone()));
        // 双写理由同 opencode / codex：`session_send` 这类命令要按
        // `State<DshRuntime>` 直接拿到运行轴，注册表那边给"按 id 分发"用。
        // 两边是同一个实例（内部 `Arc<Inner>`），不会各起一个 dsh 进程。
        ctx.manage(runtime.clone());
        ctx.register_engine(Arc::new(runtime));
        Ok(())
    }
}

/// 引擎宿主：把前面各引擎插件登记的实例收成**一份**注册表。
///
/// 这插件必须排在所有 `engine.*` 之后——它不声明 `requires`，因为"至少装一个
/// 引擎"不是硬依赖，而是运行时事实：一个引擎都没装的话注册表就是空的，
/// `engine_health` 自然返回空列表，不该让整个应用起不来。
pub struct EngineHost;

impl Plugin for EngineHost {
    fn id(&self) -> &'static str {
        "engine.host"
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        let engines = ctx.engines().to_vec();
        let mut registry = EngineRegistry::new();
        for engine in &engines {
            registry.register(engine.clone());
        }
        ctx.manage(Mutex::new(registry));

        // 退出时逐个关停（结束 serve / app-server 子进程与事件线程）。
        // 撤销栈逆序执行，所以这一条会晚于 tasks / routines 的收尾——引擎是
        // 它们的使用方，得等使用方收手之后才能拆。
        ctx.on_shutdown(move || {
            for engine in engines.iter().rev() {
                engine.shutdown();
            }
        });
        Ok(())
    }
}

/// 菜单栏托盘：状态行 + 打开 / 退出。
///
/// 放在清单里而不是 `setup` 里裸调，是为了让「关窗不退出」这条语义有个名字：
/// 关掉这个插件，应用就退化成普通窗口程序（关窗即退、没有常驻）。同时它也是
/// 最后一个还在模块内部自己 `app.manage` 的地方——现在状态句柄由这里托管，
/// 全仓只剩下内核一条装配路径。
pub struct Tray {
    app: AppHandle,
}

impl Tray {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Plugin for Tray {
    fn id(&self) -> &'static str {
        "tray"
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        // 建不起来就中止装配：没有托盘的 Prism 会变成"关窗即退出"的普通应用，
        // 那和用户以为的"常驻"不是一回事，静默降级比报错更坏。
        let status = crate::tray::setup(&self.app).map_err(|error| error.to_string())?;
        ctx.manage(status);
        Ok(())
    }
}

/// 额度缓存（只读数据层，无副作用）。
pub struct Quota;

impl Plugin for Quota {
    fn id(&self) -> &'static str {
        "quota"
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        ctx.manage(QuotaState::new());
        Ok(())
    }
}

/// 后台任务：`run_command(background)` 与终端页共用同一条通道。
pub struct Tasks;

impl Plugin for Tasks {
    fn id(&self) -> &'static str {
        "tasks"
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        let state = TasksState::new();
        // 退出时要把还活着的子进程连进程组一起收掉，否则终端里会留孤儿。
        let for_shutdown = state.clone();
        ctx.manage(state);
        ctx.on_shutdown(move || for_shutdown.shutdown());
        Ok(())
    }
}

/// 定时任务：调度线程 + 运行中标记。
pub struct Routines {
    app: AppHandle,
}

impl Routines {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Plugin for Routines {
    fn id(&self) -> &'static str {
        "routines"
    }

    fn requires(&self) -> &'static [&'static str] {
        // 触发时要按引擎名从注册表取 runtime。
        &["engine.host"]
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        ctx.manage(RoutinesState::new());
        // 调度线程随进程退出而结束，没有需要单独回收的资源。
        scheduler::start(self.app.clone());
        Ok(())
    }
}

/// 手机链接：LAN 配对对话服务 + 企业微信 webhook 推送。
///
/// 与 routines 一样是「驱动引擎运行轴」的一方，所以声明依赖 `engine.host`。
/// 起服失败（比如端口被占）只记日志不拦装配——设置页里能看到服务没在跑，
/// 用户重新开关一次即可重试，不该让整个应用起不来。
pub struct PhoneLink {
    app: AppHandle,
}

impl PhoneLink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Plugin for PhoneLink {
    fn id(&self) -> &'static str {
        "phone.link"
    }

    fn requires(&self) -> &'static [&'static str] {
        // 手机回合与权限答复都要按引擎名从注册表取 runtime。
        &["engine.host"]
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        ctx.manage(phone_link::PhoneLinkState::new());
        // 配置里开着就随应用启动服务。
        if let Err(error) = phone_link::boot(&self.app) {
            eprintln!("[prism] 手机链接服务启动失败：{error}");
        }
        let app = self.app.clone();
        ctx.on_shutdown(move || phone_link::shutdown(&app));
        Ok(())
    }
}

/// 外部工具：把 `~/.prism/mcp.json` 里的 MCP server 接成原生工具。
///
/// 这是"工具也是插件"的落地：内置工具编译进二进制、走静态表；外部工具由
/// 插件在**运行时**登记进 [`ToolRegistry`]，关停时整批撤销。两者在模型眼里
/// 没有区别（同一份 schema 列表），在信任上必须区别对待（来源 + 默认关 +
/// 每次调用过审批闸门）。
pub struct McpTools;

impl Plugin for McpTools {
    fn id(&self) -> &'static str {
        "tools.mcp"
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        let servers = mcp_client::load_servers();
        if servers.is_empty() {
            // 没配置就什么都不做：不留空转线程，也不假装"装了个插件"。
            return Ok(());
        }

        let clients: Arc<Mutex<Vec<Arc<StdioClient>>>> = Arc::new(Mutex::new(Vec::new()));
        let owners: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        {
            let clients = clients.clone();
            let owners = owners.clone();
            let stop = stop.clone();
            // 后台接：MCP server 首次启动可能要下载包（npx），别拖住应用启动。
            // 工具晚几秒出现在模型清单里，比应用起不来好。
            std::thread::spawn(move || {
                for (name, spec) in servers {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match StdioClient::start(&name, &spec) {
                        Ok(client) => {
                            let count = register_server_tools(&name, &spec, client.clone());
                            if let Ok(mut list) = owners.lock() {
                                list.push(format!("mcp.{name}"));
                            }
                            if let Ok(mut list) = clients.lock() {
                                list.push(client);
                            }
                            eprintln!("[prism] MCP {name} 接入 {count} 个工具");
                        }
                        Err(error) => eprintln!("[prism] MCP {name} 未接入：{error}"),
                    }
                }
            });
        }

        ctx.on_shutdown(move || {
            stop.store(true, Ordering::SeqCst);
            // 先撤工具（模型下一次拿到的清单里就没有了），再断连接。
            if let Ok(list) = owners.lock() {
                for owner in list.iter() {
                    ToolRegistry::global().release(owner);
                }
            }
            let drained: Vec<Arc<StdioClient>> = clients
                .lock()
                .map(|mut list| list.drain(..).collect())
                .unwrap_or_default();
            for client in drained {
                client.shutdown();
            }
        });
        Ok(())
    }
}

/// 把一段（可能是英文长句的）工具描述压成一句能塞进格子的简介。
///
/// 设置页那一行只有一百来像素，MCP server 给的说明常常是一整段话。
/// 取首句、最多 18 个字，剩下的留给悬停提示和模型看的完整 schema。
fn brief(text: Option<&str>, fallback: &str) -> String {
    let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
        return fallback.to_string();
    };
    let first_line = text.lines().next().unwrap_or(text);
    let head = first_line
        .split(['。', '；', ';', '.', '!', '！', '\n'])
        .next()
        .unwrap_or(first_line)
        .trim();
    let head = if head.is_empty() { first_line } else { head };
    let mut out: String = head.chars().take(18).collect();
    if head.chars().count() > 18 {
        out.push('…');
    }
    out
}

/// 把一个 server 的工具逐个登记进注册表，返回成功条数。
///
/// 名字带服务器前缀（`fs__read_file`）：不同 server 里的同名工具是两回事，
/// 不加命名空间就会被"先注册者赢"悄悄吞掉一个。
fn register_server_tools(server: &str, spec: &ServerSpec, client: Arc<StdioClient>) -> usize {
    let tools = match client.list_tools() {
        Ok(tools) => tools,
        Err(error) => {
            eprintln!("[prism] MCP {server} 列工具失败：{error}");
            return 0;
        }
    };
    let owner = format!("mcp.{server}");
    let mut registered = 0;

    for raw in tools {
        let Some(name) = raw.get("name").and_then(Value::as_str) else {
            continue;
        };
        let full = format!("{server}__{name}");
        // schema 原样透传：MCP server 自己最清楚参数长什么样，我们不该重写它。
        let schema = json!({
            "type": "function",
            "function": {
                "name": full,
                "description": raw
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("MCP 工具"),
                "parameters": raw
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            }
        });

        let remote = name.to_string();
        let call_client = client.clone();
        let run: Arc<BoxedRun> = Arc::new(move |_ctx, args| {
            match call_client.call_tool(&remote, args) {
                Ok(text) => ToolResult::with_parts(tools::ToolOutcome::ok(text), None, None),
                Err(error) => ToolResult::fail(error),
            }
        });

        let def = ToolDef {
            name: Cow::Owned(full),
            schema: ToolSchema::Fixed(schema),
            origin: ToolOrigin::Plugin,
            // 外部工具做什么我们不知道，一律按"会写工作目录"处理：
            // 只读档直接拒，询问档每次都问。
            permission: Permission::WriteWorkspace,
            action: Some("mcp"),
            resource_key: None,
            summary: "调用外部 MCP 工具需要授权",
            blurb: Cow::Owned(brief(
                raw.get("description").and_then(Value::as_str),
                &format!("来自 {server}"),
            )),
            // 不交给子代理：外部工具的能力边界说不清，先不扩散。
            delegable: false,
            enabled_by_default: spec.trusted,
            // 外部工具的能力边界我们不知道：一律按"只读以外"标成 write，
            // 网络按 outbound（MCP server 通常要联网），沙箱不声明。
            meta: tools::ToolMeta {
                tags: &["mcp"],
                risk: tools::Risk::Write,
                network: tools::Network::Outbound,
                sandbox: None,
                scope: tools::ToolScope::Any,
                needs: &[],
            },
            run: ToolImpl::Boxed(run),
        };

        match ToolRegistry::global().register(&owner, Arc::new(def)) {
            Ok(()) => registered += 1,
            Err(error) => eprintln!("[prism] {error}"),
        }
    }
    registered
}

/// DAG 的**引擎**执行器：把一个外部引擎接成节点执行器插件。
///
/// 每个引擎一个插件实例，而不是塞进 `dag.executor.native` 里一起管——这样
/// 「我不想让编排用 Codex」只关一个开关，`engine.codex` 的聊天照常可用；
/// 反过来关掉 `engine.codex`，这个执行器跟着级联关掉，不会留一个指向空引擎
/// 的注册项。`requires` 声明的就是这层因果。
pub struct EngineExecutor {
    id: &'static str,
    requires: &'static [&'static str],
    engine: &'static str,
}

impl EngineExecutor {
    pub const fn new(
        id: &'static str,
        requires: &'static [&'static str],
        engine: &'static str,
    ) -> Self {
        Self {
            id,
            requires,
            engine,
        }
    }
}

impl Plugin for EngineExecutor {
    fn id(&self) -> &'static str {
        self.id
    }

    fn requires(&self) -> &'static [&'static str] {
        self.requires
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        // 从"已登记的引擎"里取，而不是再去 new 一个：多引擎各自 manage 一个实例
        // 就会变成两条运行轴（两个 serve 进程），事件与打断都各说各话。
        let engine = ctx
            .engines()
            .iter()
            .find(|engine| engine.id() == self.engine)
            .cloned()
            .ok_or_else(|| format!("{} 依赖的 {} 引擎没登记", self.id, self.engine))?;
        let runner = std::sync::Arc::new(crate::dag::engine_runner::EngineRunner::new(engine));
        // 执行器 id 就是引擎 id（`EngineRunner::id` 直接转给引擎），撤销时按它找。
        let runner_id = self.engine;
        crate::dag::runner::register(runner);
        ctx.on_shutdown(move || crate::dag::runner::unregister(runner_id));
        Ok(())
    }
}

/// DAG 节点执行器：把「一个 Agent + 一段任务」跑成「一段报告」。
///
/// 它本身不调度——调度在 `dag::executor` 里，与执行器无关。这个插件只做一件事：
/// 往执行器注册表里登记 `native` 执行器，并登记撤销。关掉它，编排页的「运行」
/// 会直接报「没有节点执行器」，而不是跑到一半才发现没人干活。
pub struct DagExecutorPlugin {
    app: AppHandle,
}

impl DagExecutorPlugin {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Plugin for DagExecutorPlugin {
    fn id(&self) -> &'static str {
        "dag.executor.native"
    }

    fn requires(&self) -> &'static [&'static str] {
        // 执行器是"用引擎"的那一层：引擎不在，它连模型都问不到。
        &["engine.native"]
    }

    fn setup(&self, ctx: &mut KernelCtx<'_>) -> Result<(), String> {
        let runner = Arc::new(crate::dag::runner::NativeRunner::new(Some(self.app.clone())));
        crate::dag::runner::register(runner);
        ctx.on_shutdown(|| crate::dag::runner::unregister("native"));
        Ok(())
    }
}
