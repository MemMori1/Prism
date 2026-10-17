mod engines;
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
            ipc::search_stats,
            ipc::search_update_index,
            ipc::search_query,
            ipc::workshop_assets,
            ipc::workshop_read,
            ipc::open_path,
            ipc::workshop_mcp,
            ipc::mcp_probe,
            ipc::routines_list,
            ipc::routine_create,
            ipc::routine_update,
            ipc::routine_delete,
            ipc::routine_run,
            ipc::routine_logs,
            ipc::tray_status,
        ])
        .setup(|app| {
            app.manage(Runtime::new(app.handle().clone()));
            app.manage(CodexRuntime::new(app.handle().clone()));
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
