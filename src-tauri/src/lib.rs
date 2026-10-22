mod agents;
mod annotations;
mod artifact;
mod catalog;
mod changes;
mod dag;
mod engines;
mod files;
mod gitinfo;
mod ipc;
mod kernel;
mod mcp_client;
mod plugins;
mod routines;
mod tasks;
mod tray;

use std::sync::{Arc, Mutex};

use kernel::{Kernel, KernelState, Plugin};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            ipc::engine_health,
            ipc::agents_list,
            ipc::agent_save,
            ipc::agent_delete,
            ipc::dag_list,
            ipc::dag_get,
            ipc::dag_new,
            ipc::dag_save,
            ipc::dag_delete,
            ipc::dag_validate,
            ipc::dag_artifacts,
            ipc::dag_artifact_get,
            ipc::dag_plan,
            ipc::dag_run,
            ipc::dag_run_cancel,
            ipc::dag_run_approve,
            ipc::dag_run_latest,
            ipc::dag_run_get,
            ipc::dag_runners,
            ipc::plugins_list,
            ipc::plugin_set_enabled,
            ipc::cli_version,
            ipc::dsh_permission,
            ipc::dsh_set_permission,
            ipc::list_projects,
            ipc::list_sessions,
            ipc::session_summary,
            ipc::read_timeline,
            ipc::read_tool_detail,
            ipc::runtime_status,
            ipc::session_create,
            ipc::session_send,
            ipc::session_interrupt,
            ipc::session_permissions,
            ipc::session_permission_reply,
            ipc::engine_catalog,
            ipc::session_shell,
            ipc::session_command,
            ipc::project_files,
            ipc::changes_list,
            ipc::changes_diff,
            ipc::file_text,
            ipc::reveal_path,
            ipc::write_text,
            ipc::dir_exists,
            ipc::context_stats,
            ipc::git_snapshot,
            ipc::git_branches,
            ipc::git_switch_branch,
            ipc::search_stats,
            ipc::search_update_index,
            ipc::search_query,
            ipc::usage_summary,
            ipc::quota_summary,
            ipc::workshop_assets,
            ipc::workshop_read,
            ipc::open_path,
            ipc::read_artifact,
            ipc::annotations_get,
            ipc::annotation_update,
            ipc::tag_create,
            ipc::tag_update,
            ipc::tag_delete,
            ipc::tag_merge,
            ipc::git_info_batch,
            ipc::workshop_mcp,
            ipc::mcp_probe,
            ipc::routines_list,
            ipc::routine_create,
            ipc::routine_update,
            ipc::routine_delete,
            ipc::routine_run,
            ipc::routine_logs,
            ipc::tray_status,
            ipc::native_config,
            ipc::native_config_set,
            ipc::native_tool_toggle,
            ipc::native_probe,
            ipc::native_provider_save,
            ipc::native_provider_delete,
            ipc::native_model_select,
            ipc::native_models_fetch,
            ipc::tool_list,
            ipc::task_spawn,
            ipc::task_list,
            ipc::task_stop,
            ipc::task_remove,
        ])
        .setup(|app| {
            // 装配：一张清单，顺序即依赖顺序。加能力只改这张表——
            // 不再往 `setup` 里塞 `app.manage(...)`，也不会漏掉关停。
            //
            // 引擎插件都排在 `EngineHost` 之前：它们只把实例登记给内核，
            // 由宿主统一收成**一份**注册表（谁装了引擎由活动集决定，
            // 而不是由哪个插件抢先 `new` 出来决定）。
            let capabilities: Vec<Arc<dyn Plugin>> = vec![
                Arc::new(plugins::OpencodeEngine::new(app.handle().clone())),
                Arc::new(plugins::CodexEngine::new(app.handle().clone())),
                Arc::new(plugins::DshEngine::new(app.handle().clone())),
                Arc::new(plugins::NativeEnginePlugin),
                Arc::new(plugins::ClaudeEnginePlugin),
                Arc::new(plugins::EngineHost),
                Arc::new(plugins::Tray::new(app.handle().clone())),
                Arc::new(plugins::Quota),
                Arc::new(plugins::Tasks),
                // 外部工具：读 `~/.prism/mcp.json`，没配置就什么都不做。
                Arc::new(plugins::McpTools),
                Arc::new(plugins::Routines::new(app.handle().clone())),
                // 编排的执行器：往注册表里登记 native，没有它「运行」会直接说不支持。
                Arc::new(plugins::DagExecutorPlugin::new(app.handle().clone())),
                // 外部引擎执行器：每个引擎一个开关，关掉某个不影响别的。
                Arc::new(plugins::EngineExecutor::new(
                    "dag.executor.codex",
                    &["engine.codex"],
                    "codex",
                )),
                Arc::new(plugins::EngineExecutor::new(
                    "dag.executor.dsh",
                    &["engine.dsh"],
                    "dsh",
                )),
                Arc::new(plugins::EngineExecutor::new(
                    "dag.executor.opencode",
                    &["engine.opencode"],
                    "opencode",
                )),
            ];

            let mut kernel = Kernel::new(app.handle().clone());
            kernel
                .install(&capabilities)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
            // 装配器不留在托管状态里（它持有 AppHandle，留着就是引用环）；
            // 只留撤销栈，退出时逆序执行。
            app.manage(Mutex::new(kernel.into_state()));
            // 编排运行表：进程内共享（运行线程、IPC 命令、事件推送都看它）。
            // 放进内核之外，是因为它不参与插件装配——它是"用插件"的那一层。
            app.manage(dag::DagHub::default());

            Ok(())
        })
        .on_window_event(|window, event| {
            // 关窗 ≠ 退出：隐藏窗口，进程与定时调度继续跑；退出走托盘菜单或 Cmd+Q。
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if !tray::is_quitting() {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Prism")
        .run(|app_handle, event| match event {
            tauri::RunEvent::ExitRequested { .. } => {
                // Cmd+Q / 系统注销：放行退出，并解除关窗拦截。
                tray::mark_quitting();
            }
            // Reopen 只有 macOS / iOS 有（点 Dock 图标唤回窗口），Windows 上这个
            // 变体不存在，不加 cfg 会直接编译失败。
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => {
                tray::show_main(app_handle);
            }
            tauri::RunEvent::Exit => {
                // 逆序执行撤销栈：后装配的先拆（子进程 / 线程先收，状态后清）。
                if let Some(kernel) = app_handle.try_state::<Mutex<KernelState>>() {
                    if let Ok(mut kernel) = kernel.lock() {
                        kernel.shutdown();
                    }
                }
            }
            _ => {}
        });
}
