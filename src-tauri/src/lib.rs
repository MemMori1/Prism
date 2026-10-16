mod engines;
mod ipc;

use engines::opencode::runtime::Runtime;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            ipc::engine_health,
            ipc::list_projects,
            ipc::list_sessions,
            ipc::read_timeline,
            ipc::read_tool_detail,
            ipc::runtime_status,
            ipc::session_create,
            ipc::session_send,
            ipc::session_interrupt,
        ])
        .setup(|app| {
            app.manage(Runtime::new(app.handle().clone()));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Prism")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(runtime) = app_handle.try_state::<Runtime>() {
                    runtime.shutdown();
                }
            }
        });
}
