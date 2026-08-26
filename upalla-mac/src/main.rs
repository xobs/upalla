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
    pb_enabled: Cell<bool>,
    rec_enabled: Cell<bool>,
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

        #[unsafe(method(playbackToggled:))]
        fn playback_toggled(&self, _sender: &AnyObject) {
            // AppKit already flipped the checkbox — read its new state.
            if let Some(controls) = self.ivars().controls.get() {
                self.apply_playback(controls.playback_enabled());
            }
        }

        #[unsafe(method(recordingToggled:))]
        fn recording_toggled(&self, _sender: &AnyObject) {
            if let Some(controls) = self.ivars().controls.get() {
                self.apply_recording(controls.recording_enabled());
            }
        }

        #[unsafe(method(togglePlayback:))]
        fn toggle_playback(&self, _sender: &AnyObject) {
            // Tray click — flip the checkbox ourselves, then apply.
            if let Some(controls) = self.ivars().controls.get() {
                let enabled = !controls.playback_enabled();
                controls.set_playback_enabled(enabled);
                self.apply_playback(enabled);
            }
        }

        #[unsafe(method(toggleRecording:))]
        fn toggle_recording(&self, _sender: &AnyObject) {
            if let Some(controls) = self.ivars().controls.get() {
                let enabled = !controls.recording_enabled();
                controls.set_recording_enabled(enabled);
                self.apply_recording(enabled);
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
    /// Pushes a chain's enabled state to the engine and mirrors it in the tray.
    fn apply_playback(&self, enabled: bool) {
        self.ivars().pb_enabled.set(enabled);
        let _ = self.ivars().cmd_tx.send(Cmd::SetPlaybackBypass(!enabled));
        if let Some(tray) = self.ivars().tray.get() {
            tray.pb_enabled_item.setState(check_state(enabled));
        }
    }

    fn apply_recording(&self, enabled: bool) {
        self.ivars().rec_enabled.set(enabled);
        let _ = self.ivars().cmd_tx.send(Cmd::SetRecordingBypass(!enabled));
        if let Some(tray) = self.ivars().tray.get() {
            tray.rec_enabled_item.setState(check_state(enabled));
        }
    }

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
            pb_enabled: Cell::new(true),
            rec_enabled: Cell::new(true),
        });
        unsafe { msg_send![super(this), init] }
    }
}

fn check_state(on: bool) -> objc2_app_kit::NSControlStateValue {
    if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    }
}

fn create_window(mtm: MainThreadMarker, delegate: &AppDelegate) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(controls::WINDOW_W, controls::WINDOW_H),
            ),
            // Not resizable: the layout uses fixed frames sized to fit exactly,
            // so a resize would only open dead space rather than reflow anything.
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("Upalla"));

    let view = window.contentView().expect("window must have content view");
    let controls = controls::build_controls(mtm, &view, delegate);
    let _ = delegate.ivars().controls.set(controls);
    let size = NSSize::new(controls::WINDOW_W, controls::WINDOW_H);
    window.setContentMinSize(size);
    window.setContentMaxSize(size);

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
