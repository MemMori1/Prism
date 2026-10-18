mod annotations;
mod artifact;
mod catalog;
mod changes;
mod engines;
mod files;
mod gitinfo;
mod ipc;
mod routines;
mod tray;

use engines::codex::runtime::Runtime as CodexRuntime;
use engines::opencode::runtime::Runtime;
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
            ipc::cli_version,
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
            ipc::native_probe,
            ipc::native_provider_save,
            ipc::native_provider_delete,
            ipc::native_model_select,
            ipc::native_models_fetch,
        ])
        .setup(|app| {
            app.manage(Runtime::new(app.handle().clone()));
            app.manage(CodexRuntime::new(app.handle().clone()));
            app.manage(engines::quota::QuotaState::new());
            app.manage(routines::scheduler::RoutinesState::new());
            routines::scheduler::start(app.handle().clone());
            tray::setup(app.handle())?;
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
            tauri::RunEvent::Reopen { .. } => {
                // Dock 图标被点击：唤起主窗口（macOS）。
                tray::show_main(app_handle);
            }
            tauri::RunEvent::Exit => {
                if let Some(runtime) = app_handle.try_state::<Runtime>() {
                    runtime.shutdown();
                }
                if let Some(codex) = app_handle.try_state::<CodexRuntime>() {
                    codex.shutdown();
                }
            }
            _ => {}
        });
}
