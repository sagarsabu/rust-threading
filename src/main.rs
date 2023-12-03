mod errors;
mod logging;
mod scoped_deadline;
mod threading;
mod worker_thread;

use crate::{
    threading::SageThread,
    worker_thread::{Worker, WorkerEvent},
};

fn setup_logger() -> Result<(), log::SetLoggerError> {
    let logger = Box::new(logging::Logger::new(log::Level::Debug));
    log::set_max_level(logger.get_level_filter());
    log::set_boxed_logger(logger)?;

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    setup_logger()?;

    {
        let mut worker = SageThread::<Worker>::new("worker-A");
        worker.start().expect("Failed to start worker");
        worker.transmit_event(WorkerEvent::TestA);
        worker.transmit_event(WorkerEvent::TestB);

        log::info!("sleeping for 100ms");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    Ok(())
}
