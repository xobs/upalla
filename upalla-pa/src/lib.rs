use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};
use upalla_core::model::Model;

mod filter;

pub use filter::{DeviceInfo, DeviceLists, Status};
use filter::Cmd;

pub struct PaFilter {
    cmd_tx: Sender<Cmd>,
    status_rx: Receiver<Status>,
    bypass: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PaFilter {
    pub fn new(model: Model) -> Result<Self> {
        let (cmd_tx, cmd_rx) = bounded::<Cmd>(8);
        let (status_tx, status_rx) = bounded::<Status>(8);
        let bypass = Arc::new(AtomicBool::new(false));
        let bypass_clone = bypass.clone();

        let handle = std::thread::Builder::new()
            .name("upalla-pa".into())
            .spawn(move || {
                if let Err(e) = filter::run_filter(model, cmd_rx, status_tx, bypass_clone) {
                    log::error!("PA filter thread error: {e}");
                }
            })?;

        Ok(PaFilter {
            cmd_tx,
            status_rx,
            bypass,
            handle: Some(handle),
        })
    }

    pub fn set_bypass(&self, bypass: bool) {
        self.bypass.store(bypass, Ordering::Relaxed);
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
}

impl Drop for PaFilter {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Cmd::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
