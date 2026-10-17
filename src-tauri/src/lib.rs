mod engines;
mod ipc;

use engines::codex::runtime::Runtime as CodexRuntime;
use engines::opencode::runtime::Runtime;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
        ])
        .setup(|app| {
            app.manage(Runtime::new(app.handle().clone()));
            app.manage(CodexRuntime::new(app.handle().clone()));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Prism")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(runtime) = app_handle.try_state::<Runtime>() {
                    runtime.shutdown();
                }
                if let Some(codex) = app_handle.try_state::<CodexRuntime>() {
                    codex.shutdown();
                }
            }
        });
}
