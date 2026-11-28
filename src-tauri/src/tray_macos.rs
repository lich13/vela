//! Transient NSMenu attachment for macOS 27, as used by Sub2Ops 4d0eafc1.
use crate::errors::code;
use crate::storage::{AppError, Result};
use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::NSMenu;
use objc2_foundation::{NSArray, NSRunLoop, NSRunLoopCommonModes};
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CString};
use tauri::Manager;
extern "C" {
    fn vela_tray_menu_style(menu: *const c_void, first: isize, styles: *const c_char);
}
thread_local! { static CONTEXT_MENU: RefCell<Option<Retained<NSMenu>>> = const { RefCell::new(None) }; }
// Whether a menu is waiting to open or is open. Events are handled meanwhile,
// another press on the icon among them, and two presses may be handled before
// the menu of the first has opened.
thread_local! { static OPEN: Cell<bool> = const { Cell::new(false) }; }
pub fn install(tray: &tauri::tray::TrayIcon) -> Result<()> {
    tray.with_inner_tray_icon(|inner| -> Result<()> {
        let main =
            MainThreadMarker::new().ok_or_else(|| AppError::new(code::TRAY, "托盘需要主线程"))?;
        inner.set_show_menu_on_right_click(false);
        let status = inner
            .ns_status_item()
            .ok_or_else(|| AppError::new(code::TRAY, "托盘图标不可用"))?;
        crate::quick_macos::attach_status_item(Retained::as_ptr(&status).cast());
        CONTEXT_MENU.with(|saved| saved.replace(status.menu(main)));
        status.setMenu(None);
        Ok(())
    })
    .map_err(|_| AppError::new(code::TRAY, "无法初始化托盘菜单"))?
}
/// Draws the items of the icon's menu from `first` on the way `styles` says,
/// one for each item in their order: see `native/tray_menu.swift`. An item
/// gets the attributed title its style makes, or loses the one it had. Called
/// on the main thread, where `install` saved the menu. Before `install` there
/// is no menu to reach and nothing is done, so the items are styled once more
/// after it.
pub fn style(first: usize, styles: &[impl serde::Serialize]) {
    let Some(menu) = CONTEXT_MENU.with(|saved| saved.borrow().clone()) else {
        return;
    };
    let Some(styles) = serde_json::to_string(styles)
        .ok()
        .and_then(|json| CString::new(json).ok())
    else {
        return;
    };
    // SAFETY: the menu is held for as long as the call lasts, on the thread
    // that alone touches it. The text is a C string that outlives the call,
    // which keeps neither.
    unsafe {
        vela_tray_menu_style(
            Retained::as_ptr(&menu).cast(),
            first as isize,
            styles.as_ptr(),
        );
    }
}
/// Opens the icon's menu once the event that asked for it has been handled.
/// The click that opens it returns when the menu closes. Made inside the
/// handler it would keep the event loop from its own queue for that long, and
/// whatever calls the main thread meanwhile would wait: the rows of the menu
/// that change while it is open, and every service that asks after a window.
/// A press that comes before the menu of an earlier one has closed opens
/// nothing more.
pub fn context_menu(tray: &tauri::tray::TrayIcon) {
    if OPEN.replace(true) {
        return;
    }
    let tray = tray.clone();
    let open = block2::RcBlock::new(move || {
        if let Err(error) = open(&tray) {
            crate::emit_switch_error(tray.app_handle(), error);
        }
        OPEN.set(false);
    });
    // SAFETY: the main run loop runs the block on this thread, the main one.
    // The modes are a constant of Foundation.
    unsafe {
        NSRunLoop::mainRunLoop()
            .performInModes_block(&NSArray::from_slice(&[NSRunLoopCommonModes]), &open);
    }
}
fn open(tray: &tauri::tray::TrayIcon) -> Result<()> {
    let app = tray.app_handle();
    if crate::quick_macos::is_visible()
        && app
            .try_state::<crate::quick::Panel>()
            .is_some_and(|panel| !panel.snapshot().pinned)
    {
        crate::quick_macos::hide(app);
    }
    tray.with_inner_tray_icon(|inner| -> Result<()> {
        let main =
            MainThreadMarker::new().ok_or_else(|| AppError::new(code::TRAY, "托盘需要主线程"))?;
        let status = inner
            .ns_status_item()
            .ok_or_else(|| AppError::new(code::TRAY, "托盘图标不可用"))?;
        let button = status
            .button(main)
            .ok_or_else(|| AppError::new(code::TRAY, "托盘按钮不可用"))?;
        let menu = CONTEXT_MENU
            .with(|saved| saved.borrow().clone())
            .ok_or_else(|| AppError::new(code::TRAY, "托盘菜单不可用"))?;
        status.setMenu(Some(&menu));
        unsafe {
            button.performClick(None);
        }
        status.setMenu(None);
        Ok(())
    })
    .map_err(|_| AppError::new(code::TRAY, "无法打开托盘菜单"))?
}
