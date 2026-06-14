#[cfg(target_os = "linux")]
mod linux_impl {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;

    use crossbeam_channel::Sender;
    use tray_icon::menu::Menu;
    use tray_icon::menu::MenuItem;
    use tray_icon::TrayIconBuilder;

    use super::TrayMenuIds;

    pub fn run_tray(done: Arc<AtomicBool>, ids_tx: Sender<TrayMenuIds>) {
        gtk::init().expect("gtk init");

        let menu = Menu::new();

        let show_hide = MenuItem::new("Show", true, None);
        let enabled = MenuItem::new("Enabled", true, None);
        let quit = MenuItem::new("Quit", true, None);

        let ids = TrayMenuIds {
            show_hide: show_hide.id().clone(),
            enabled: enabled.id().clone(),
            quit: quit.id().clone(),
        };

        menu.append(&show_hide).expect("append");
        menu.append(&enabled).expect("append");
        menu.append(&quit).expect("append");

        let icon =
            tray_icon::Icon::from_rgba(vec![0u8; 64 * 64 * 4], 64, 64).expect("create icon");

        let _tray = TrayIconBuilder::new()
            .with_tooltip("Upalla")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .build()
            .expect("build tray");

        let _ = ids_tx.send(ids);

        while !done.load(Ordering::Relaxed) {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux_impl::run_tray;

#[derive(Debug, Clone)]
pub struct TrayMenuIds {
    pub show_hide: tray_icon::menu::MenuId,
    pub enabled: tray_icon::menu::MenuId,
    pub quit: tray_icon::menu::MenuId,
}

#[cfg(not(target_os = "linux"))]
pub fn run_tray(
    _done: Arc<std::sync::atomic::AtomicBool>,
    _ids_tx: crossbeam_channel::Sender<TrayMenuIds>,
) {
    std::thread::sleep(std::time::Duration::from_secs(u64::MAX));
}
