use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};
use upalla_core::model::Model;

#[cfg(target_os = "linux")]
mod filter;

#[cfg(target_os = "macos")]
mod filter_ca;

#[cfg(target_os = "linux")]
use filter::Cmd;
#[cfg(target_os = "linux")]
pub use filter::{DeviceInfo, DeviceLists, Status};

#[cfg(target_os = "macos")]
use filter_ca::Cmd;
#[cfg(target_os = "macos")]
pub use filter_ca::{DeviceInfo, DeviceLists, Status};

pub struct PaFilter {
    cmd_tx: Sender<Cmd>,
    status_rx: Receiver<Status>,
    playback_enabled: Arc<AtomicBool>,
    recording_enabled: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl PaFilter {
    pub fn new(
        model: Model,
        playback_enabled: Arc<AtomicBool>,
        recording_enabled: Arc<AtomicBool>,
    ) -> Result<Self> {
        let (cmd_tx, cmd_rx) = bounded::<Cmd>(8);
        let (status_tx, status_rx) = bounded::<Status>(8);

        let handle = std::thread::Builder::new()
            .name("upalla-audio".into())
            .spawn({
                let playback_enabled = Arc::clone(&playback_enabled);
                let recording_enabled = Arc::clone(&recording_enabled);
                move || {
                    #[cfg(target_os = "linux")]
                    {
                        if let Err(e) = filter::run_filter(
                            model,
                            cmd_rx,
                            status_tx,
                            playback_enabled,
                            recording_enabled,
                        ) {
                            log::error!("PA filter thread error: {e}");
                        }
                    }
                    #[cfg(target_os = "macos")]
                    {
                        if let Err(e) = filter_ca::run_filter(
                            model,
                            cmd_rx,
                            status_tx,
                            playback_enabled,
                            recording_enabled,
                        ) {
                            log::error!("CA filter thread error: {e}");
                        }
                    }
                }
            })?;

        Ok(PaFilter {
            cmd_tx,
            status_rx,
            playback_enabled,
            recording_enabled,
            handle,
        })
    }

    /// Set bypass for both chains together (used by the standalone filter binary).
    pub fn set_bypass(&self, bypass: bool) {
        self.playback_enabled.store(!bypass, Ordering::Relaxed);
        self.recording_enabled.store(!bypass, Ordering::Relaxed);
    }

    /// True if both chains are bypassed.
    pub fn bypass(&self) -> bool {
        !self.playback_enabled.load(Ordering::Relaxed)
            && !self.recording_enabled.load(Ordering::Relaxed)
    }

    /// Set bypass for the playback (sink) chain only.
    pub fn set_playback_bypass(&self, bypass: bool) {
        self.playback_enabled.store(!bypass, Ordering::Relaxed);
    }

    pub fn playback_bypass(&self) -> bool {
        !self.playback_enabled.load(Ordering::Relaxed)
    }

    /// Set bypass for the recording (src) chain only.
    pub fn set_recording_bypass(&self, bypass: bool) {
        self.recording_enabled.store(!bypass, Ordering::Relaxed);
    }

    pub fn recording_bypass(&self) -> bool {
        !self.recording_enabled.load(Ordering::Relaxed)
    }

    pub fn switch_model(&self, model: Model) {
        let _ = self.cmd_tx.send(Cmd::SwitchModel(model));
    }

    pub fn enumerate_devices(&self) -> DeviceLists {
        let (tx, rx) = bounded(1);
        let _ = self.cmd_tx.send(Cmd::EnumerateDevices(tx));
        rx.recv().unwrap_or_else(|_| DeviceLists {
            sinks: Vec::new(),
            sources: Vec::new(),
            default_sink: String::new(),
            default_source: String::new(),
        })
    }

    pub fn set_sink(&self, name: String) {
        let _ = self.cmd_tx.send(Cmd::SetSink(name));
    }

    pub fn set_source(&self, name: String) {
        let _ = self.cmd_tx.send(Cmd::SetSource(name));
    }

    pub fn status_receiver(&self) -> &Receiver<Status> {
        &self.status_rx
    }

    pub fn shutdown(&self) -> bool {
        let _ = self.cmd_tx.send(Cmd::Shutdown);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !self.handle.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.handle.is_finished()
    }
}
