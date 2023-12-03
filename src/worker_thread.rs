use crate::{errors::WorkerError, threading};

pub enum WorkerEvent {
    TestA,
    TestB,
}

pub struct WorkerThread {}

impl threading::Worker for WorkerThread {
    type Event = WorkerEvent;

    fn starting() {
        log::info!("starting worker thread");
    }

    fn handle_event(event: Self::Event) -> Result<(), WorkerError> {
        match event {
            WorkerEvent::TestA => log::info!("got event - a"),
            WorkerEvent::TestB => log::info!("got event - b"),
        }

        Ok(())
    }

    fn stopping() {
        log::info!("stopping worker thread");
    }
}
