//! 托盘（macOS 菜单栏项）：状态一眼可见、可唤起窗口、可退出。
//!
//! 轻量自研：用 Tauri 内置 tray，不引入独立常驻二进制 + 跨进程管道的系统级基建。
//! 配套语义「关窗 ≠ 退出」：窗口关闭只是隐藏，进程与定时调度继续跑；
//! 真正退出走托盘菜单「退出 Prism」或 Cmd+Q。
//!
//! 状态来源：前端（工作台 + 归档视图汇总）通过 `tray_status` IPC 实时推送，
//! 这里不复制引擎状态，只做展示。

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager,
};

/// 托盘 id（`app.tray_by_id` 用）。
const TRAY_ID: &str = "prism-tray";
/// 状态行菜单项 id。
const STATUS_ITEM_ID: &str = "tray-status";

/// 退出流程已开始（托盘退出 / Cmd+Q / 系统注销）：窗口关闭不再拦截。
static QUITTING: AtomicBool = AtomicBool::new(false);

/// 状态行菜单项句柄：供 `update_status` 实时改文案。
pub struct StatusItem(pub MenuItem<tauri::Wry>);

/// 状态文案（纯函数，便于单测）。
pub fn status_text(running: u32, waiting: u32) -> String {
    match (running, waiting) {
        (0, 0) => "空闲".to_string(),
        (0, waiting) => format!("{waiting} 个待审批"),
        (running, 0) => format!("{running} 个会话执行中"),
        (running, waiting) => format!("{running} 个执行中 · {waiting} 个待审批"),
    }
}

/// 标记退出已开始（不放行的话，关闭窗口会被「关窗 ≠ 退出」拦截）。
pub fn mark_quitting() {
    QUITTING.store(true, Ordering::SeqCst);
}

/// 是否正在退出。
pub fn is_quitting() -> bool {
    QUITTING.load(Ordering::SeqCst)
}

/// 真正退出应用（托盘菜单调用）。
pub fn quit(app: &AppHandle) {
    mark_quitting();
    app.exit(0);
}

/// 唤起主窗口（托盘「打开 Prism」/ Dock 图标点击）。
pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// 应用启动时装配托盘：图标（单色模板）+ 状态行 + 打开/退出。
///
/// 返回状态行句柄而不是自己 `app.manage`：交给谁托管由装配方（`Tray` 插件）
/// 决定。这样"应用状态是从哪儿来的"只有一条路径，不会在某个模块深处冒出
/// 一处裸 `manage`。
pub fn setup(app: &AppHandle) -> Result<StatusItem, Box<dyn std::error::Error>> {
    let status_item = MenuItem::with_id(app, STATUS_ITEM_ID, "空闲", false, None::<&str>)?;
    let open_item = MenuItem::with_id(app, "tray-open", "打开 Prism", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "tray-quit", "退出 Prism", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&status_item, &separator, &open_item, &quit_item])?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        .icon_as_template(true)
        .tooltip("Prism · 空闲")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray-open" => show_main(app),
            "tray-quit" => quit(app),
            _ => {}
        })
        .build(app)?;

    Ok(StatusItem(status_item))
}

/// 前端每次「执行中 / 待审批」变化时调用：刷新菜单文案、菜单栏数字与 tooltip。
pub fn update_status(app: &AppHandle, running: u32, waiting: u32) {
    let text = status_text(running, waiting);
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(format!("Prism · {text}")));
        // 有待审批时在图标旁直接显示数字，扫一眼可见；否则清掉避免常驻。
        let title = (waiting > 0).then(|| waiting.to_string());
        let _ = tray.set_title(title);
    }
    if let Some(state) = app.try_state::<StatusItem>() {
        let _ = state.0.set_text(text);
    }
}

#[cfg(test)]
mod tests {
    use super::status_text;

    #[test]
    fn status_text_covers_four_states() {
        assert_eq!(status_text(0, 0), "空闲");
        assert_eq!(status_text(0, 2), "2 个待审批");
        assert_eq!(status_text(3, 0), "3 个会话执行中");
        assert_eq!(status_text(3, 2), "3 个执行中 · 2 个待审批");
    }
}
