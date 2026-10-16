mod engines;
mod ipc;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            ipc::engine_health,
            ipc::list_projects,
            ipc::list_sessions,
            ipc::read_timeline,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Prism");
}
