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
    enabled: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl PaFilter {
    pub fn new(model: Model, enabled: Arc<AtomicBool>) -> Result<Self> {
        let (cmd_tx, cmd_rx) = bounded::<Cmd>(8);
        let (status_tx, status_rx) = bounded::<Status>(8);

        let handle = std::thread::Builder::new()
            .name("upalla-audio".into())
            .spawn({
                let enabled = Arc::clone(&enabled);
                move || {
                    #[cfg(target_os = "linux")]
                    {
                        if let Err(e) = filter::run_filter(model, cmd_rx, status_tx, enabled) {
                            log::error!("PA filter thread error: {e}");
                        }
                    }
                    #[cfg(target_os = "macos")]
                    {
                        if let Err(e) = filter_ca::run_filter(model, cmd_rx, status_tx, enabled) {
                            log::error!("CA filter thread error: {e}");
                        }
                    }
                }
            })?;

        Ok(PaFilter {
            cmd_tx,
            status_rx,
            enabled,
            handle,
        })
    }

    pub fn set_bypass(&self, bypass: bool) {
        self.enabled.store(!bypass, Ordering::Relaxed);
    }

    pub fn bypass(&self) -> bool {
        !self.enabled.load(Ordering::Relaxed)
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
