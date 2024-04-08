mod errors;
mod logging;
mod scoped_deadline;
mod signal_handler;
mod threading;

use std::sync;

use crate::errors::SageError;
use crate::threading::SageThread;

enum WorkerEvent {
    TestA,
    TestB,
}

struct Worker {}

impl threading::ThreadHandler for Worker {
    type HandlerEvent = WorkerEvent;

    fn handle_event(&self, event: Self::HandlerEvent) {
        match event {
            WorkerEvent::TestA => log::info!("got event - a"),
            WorkerEvent::TestB => log::info!("got event - b"),
        }
    }
}

fn main() -> Result<(), SageError> {
    logging::setup_logger()?;

    let worker = sync::Arc::from(SageThread::new("worker", Worker {})?);
    worker.start();

    let worker_cp = worker.clone();
    std::thread::Builder::new()
        .name("Transmitter".into())
        .spawn(move || {
            let delta = std::time::Duration::from_millis(100);
            while worker_cp.is_running() {
                worker_cp.transmit_event(WorkerEvent::TestA);
                worker_cp.transmit_event(WorkerEvent::TestB);
                log::info!("sleeping for {}ms", delta.as_millis());
                std::thread::sleep(delta);
            }
        })?;

    signal_handler::wait_for_exit(|| {
        worker.stop();
    });

    Ok(())
}
