use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crossbeam_queue::ArrayQueue;

use upalla_core::Denoiser;

pub struct GpuTask {
    pub spec_real: Vec<Vec<f32>>,
    pub spec_imag: Vec<Vec<f32>>,
    pub num_frames: usize,
}

pub struct GpuResponse {
    pub erb_mask: Vec<f32>,
    pub df_coefs: Vec<f32>,
    pub processed_frames: usize,
}

pub struct WorkerChannel {
    requests: Arc<ArrayQueue<GpuTask>>,
    responses: Arc<ArrayQueue<GpuResponse>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WorkerChannel {
    pub fn spawn(denoiser: Denoiser) -> Self {
        let requests: Arc<ArrayQueue<GpuTask>> = Arc::new(ArrayQueue::new(3));
        let responses: Arc<ArrayQueue<GpuResponse>> = Arc::new(ArrayQueue::new(3));
        let shutdown = Arc::new(AtomicBool::new(false));

        let req = requests.clone();
        let resp = responses.clone();
        let sd = shutdown.clone();
        let denoiser = Arc::new(Mutex::new(denoiser));
        let worker_thread = thread::current();

        let handle = thread::spawn(move || {
            log::info!("Upalla worker thread started");
            while !sd.load(Ordering::Relaxed) {
                if let Some(task) = req.pop() {
                    let result = {
                        let mut d = denoiser.lock().unwrap();
                        d.process_batch_direct(
                            &task.spec_real,
                            &task.spec_imag,
                            task.num_frames,
                        )
                    };

                    match result {
                        Ok((erb_mask, df_coefs)) => {
                            let response = GpuResponse {
                                erb_mask,
                                df_coefs,
                                processed_frames: task.num_frames,
                            };
                            resp.force_push(response);
                            worker_thread.unpark();
                        }
                        Err(e) => {
                            log::error!("Worker inference failed: {e}");
                        }
                    }
                } else {
                    thread::park();
                }
            }
            log::info!("Upalla worker thread stopped");
        });

        WorkerChannel {
            requests,
            responses,
            shutdown,
            thread: Some(handle),
        }
    }

    pub fn try_send(&self, task: GpuTask) {
        self.requests.force_push(task);
        if let Some(handle) = &self.thread {
            handle.thread().unpark();
        }
    }

    pub fn try_recv(&self) -> Option<GpuResponse> {
        self.responses.pop()
    }

    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = &self.thread {
            handle.thread().unpark();
        }
    }
}

impl Drop for WorkerChannel {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}
