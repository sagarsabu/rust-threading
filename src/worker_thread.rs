use crate::threading;

pub enum WorkerEvent {
    TestA,
    TestB,
}

pub struct Worker {}

impl threading::ThreadHandler for Worker {
    type HandlerEvent = WorkerEvent;

    fn starting() {
        log::info!("starting worker thread");
    }

    fn handle_event(event: WorkerEvent) {
        match event {
            WorkerEvent::TestA => log::info!("got event - a"),
            WorkerEvent::TestB => log::info!("got event - b"),
        }
    }

    fn stopping() {
        log::info!("stopping worker thread");
    }
}
