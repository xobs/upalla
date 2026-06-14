use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use upalla_core::model::Model;
use upalla_pa::PaFilter;

fn main() -> Result<()> {
    env_logger::init();
    log::info!("Upalla PulseAudio filter starting...");

    let pa = PaFilter::new(Model::default(), Arc::new(AtomicBool::new(true)))?;

    let shutting_down = Arc::new(AtomicBool::new(false));
    ctrlc::set_handler({
        let shutting_down = Arc::clone(&shutting_down);
        move || {
            if !shutting_down.swap(true, Ordering::SeqCst) {
                println!("Ctrl-C pressed, shutting down");
            }
        }
    })
    .context("ctrlc")?;

    let status_rx = pa.status_receiver();
    log::info!("PA filter started. Press Ctrl-C to stop.");

    while !shutting_down.load(Ordering::Relaxed) {
        if let Ok(status) = status_rx.recv_timeout(std::time::Duration::from_millis(250)) {
            log::debug!("rms_in={:.4} rms_out={:.4}", status.rms_in, status.rms_out);
        }
    }

    drop(pa);
    log::info!("Upalla PA filter stopped.");
    Ok(())
}
