use std::cell::{Cell, RefCell};

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSControlStateValueOff, NSControlStateValueOn, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, NSTimer,
};

mod audio;
mod blackhole;
mod controls;
mod icon;
mod tray;

use audio::{Cmd, Status};
use controls::Controls;
use tray::Tray;

struct AppDelegateIvars {
    cmd_tx: Sender<Cmd>,
    status_rx: RefCell<Receiver<Status>>,
    controls: std::cell::OnceCell<Controls>,
    tray: std::cell::OnceCell<Tray>,
    window: std::cell::OnceCell<Retained<NSWindow>>,
    bypass_enabled: Cell<bool>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppDelegateIvars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, notification: &NSNotification) {
            let mtm = self.mtm();

            let app = notification.object()
                .unwrap()
                .downcast::<NSApplication>()
                .unwrap();

            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

            let tray = tray::create_tray(mtm, self);
            self.ivars().tray.set(tray).unwrap();

            let window = create_window(mtm, self);
            unsafe { window.setReleasedWhenClosed(false) };
            window.setDelegate(Some(ProtocolObject::from_ref(self)));

            // Enumerate devices directly — fast (~1ms), never fails.
            if let Ok(devices) = audio::enumerate_devices() {
                if let Some(controls) = self.ivars().controls.get() {
                    controls.populate_devices(&devices.sinks, &devices.sources);
                }
            }
            // Also tell audio thread so it stays in sync.
            let (dev_tx, _dev_rx) = unbounded();
            let _ = self.ivars().cmd_tx.send(Cmd::EnumerateDevices(dev_tx));

            window.center();
            self.ivars().window.set(window).unwrap();

            let _timer = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    0.1,
                    self,
                    sel!(tick:),
                    None,
                    true,
                )
            };
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            false
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            let _ = self.ivars().cmd_tx.send(Cmd::Shutdown);
        }
    }

    unsafe impl NSWindowDelegate for AppDelegate {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, sender: &NSWindow) -> bool {
            sender.orderOut(None);
            false
        }
    }

    impl AppDelegate {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            use std::sync::atomic::{AtomicBool, Ordering};
            static TICKED: AtomicBool = AtomicBool::new(false);
            let mut new_status = None;
            if let Ok(rx) = self.ivars().status_rx.try_borrow_mut() {
                while let Ok(s) = rx.try_recv() {
                    new_status = Some(s);
                }
            }
            if let Some(status) = new_status {
                if !TICKED.swap(true, Ordering::Relaxed) {
                    log::info!(
                        "First status: pb_in={:.6} pb_out={:.6} rec_in={:.6} rec_out={:.6}",
                        status.playback_in, status.playback_out,
                        status.recording_in, status.recording_out,
                    );
                }
                if let Some(controls) = self.ivars().controls.get() {
                    controls.update_levels(&status);
                }
            }
        }

        #[unsafe(method(showWindow:))]
        fn show_window(&self, _sender: &AnyObject) {
            if let Some(window) = self.ivars().window.get() {
                window.makeKeyAndOrderFront(None);
                let app = NSApplication::sharedApplication(self.mtm());
                #[allow(deprecated)]
                app.activateIgnoringOtherApps(true);
            }
        }

        #[unsafe(method(checkboxToggled:))]
        fn checkbox_toggled(&self, _sender: &AnyObject) {
            // AppKit already flipped the checkbox — read its new state.
            if let Some(controls) = self.ivars().controls.get() {
                let enabled = controls.enabled();
                self.ivars().bypass_enabled.set(enabled);
                let _ = self.ivars().cmd_tx.send(Cmd::SetBypass(!enabled));
                if let Some(tray) = self.ivars().tray.get() {
                    tray.enabled_item.setState(if enabled {
                        NSControlStateValueOn
                    } else {
                        NSControlStateValueOff
                    });
                }
            }
        }

        #[unsafe(method(toggleEnabled:))]
        fn toggle_enabled(&self, _sender: &AnyObject) {
            // Tray click — flip checkbox, then read its new state.
            if let Some(controls) = self.ivars().controls.get() {
                let was_enabled = controls.enabled();
                controls.set_enabled(!was_enabled);
                let enabled = !was_enabled;
                self.ivars().bypass_enabled.set(enabled);
                let _ = self.ivars().cmd_tx.send(Cmd::SetBypass(!enabled));
                if let Some(tray) = self.ivars().tray.get() {
                    tray.enabled_item.setState(if enabled {
                        NSControlStateValueOn
                    } else {
                        NSControlStateValueOff
                    });
                }
            }
        }

        #[unsafe(method(refreshDevices:))]
        fn refresh_devices(&self, _sender: &AnyObject) {
            let (tx, rx) = unbounded();
            let _ = self.ivars().cmd_tx.send(Cmd::EnumerateDevices(tx));
            if let Ok(devices) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
                if let Some(controls) = self.ivars().controls.get() {
                    controls.populate_devices(&devices.sinks, &devices.sources);
                }
            }
        }

        #[unsafe(method(sinkSelected:))]
        fn sink_selected(&self, _sender: &AnyObject) {
            if let Some(controls) = self.ivars().controls.get() {
                let _ = self.ivars().cmd_tx.send(Cmd::SetSink(controls.selected_sink()));
            }
        }

        #[unsafe(method(sourceSelected:))]
        fn source_selected(&self, _sender: &AnyObject) {
            if let Some(controls) = self.ivars().controls.get() {
                let _ = self.ivars().cmd_tx.send(Cmd::SetSource(controls.selected_source()));
            }
        }

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: &AnyObject) {
            let _ = self.ivars().cmd_tx.send(Cmd::Shutdown);
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl AppDelegate {
    fn new(
        mtm: MainThreadMarker,
        cmd_tx: Sender<Cmd>,
        status_rx: Receiver<Status>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars {
            cmd_tx,
            status_rx: RefCell::new(status_rx),
            controls: std::cell::OnceCell::new(),
            tray: std::cell::OnceCell::new(),
            window: std::cell::OnceCell::new(),
            bypass_enabled: Cell::new(true),
        });
        unsafe { msg_send![super(this), init] }
    }
}

fn create_window(mtm: MainThreadMarker, delegate: &AppDelegate) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(420.0, 450.0)),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("Upalla"));

    let view = window.contentView().expect("window must have content view");
    let controls = controls::build_controls(mtm, &view, delegate);
    let _ = delegate.ivars().controls.set(controls);
    window.setContentMinSize(NSSize::new(420.0, 380.0));

    window
}

fn main() -> Result<()> {
    env_logger::init();

    let mtm = MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("must run on main thread"))?;

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let (cmd_tx, cmd_rx) = unbounded::<Cmd>();
    let (status_tx, status_rx) = unbounded::<Status>();

    audio::run_audio_engine(cmd_rx, status_tx);

    let delegate = AppDelegate::new(mtm, cmd_tx.clone(), status_rx);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    app.run();

    Ok(())
}
