mod errors;
mod logging;
mod scoped_deadline;
mod signal_handler;
mod threading;

use std::sync;

use crate::threading::SageThread;

fn setup_logger() -> Result<(), log::SetLoggerError> {
    let logger = Box::new(logging::Logger::new(log::Level::Debug));
    log::set_max_level(logger.get_level_filter());
    log::set_boxed_logger(logger)?;

    Ok(())
}

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    setup_logger()?;

    let worker = sync::Arc::from(SageThread::new("worker", Worker {})?);
    worker.start();

    let worker_cp = worker.clone();
    let signal_handler_thread = SageThread::new(
        "signal",
        signal_handler::ExitHandler::new(move || {
            worker_cp.stop();
        }),
    )?;
    signal_handler_thread.start();

    let delta = std::time::Duration::from_millis(100);
    while worker.is_running() {
        worker.transmit_event(WorkerEvent::TestA);
        worker.transmit_event(WorkerEvent::TestB);
        log::info!("sleeping for {}ms", delta.as_millis());
        std::thread::sleep(delta);
    }

    Ok(())
}
