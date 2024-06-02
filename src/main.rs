mod errors;
mod logging;
mod scoped_deadline;
mod signal_handler;
mod threading;
mod timer;

use std::sync::Arc;

use crate::errors::SageError;
use crate::threading::{SageThread, SageThreadControl};

enum DispatchEvent {
    Dispatch,
}

enum WorkerEvent {
    TestA,
    TestB,
}

fn main() -> Result<(), SageError> {
    logging::setup_logger()?;

    let worker = SageThreadControl::new(
        "worker",
        |_thread, event: WorkerEvent| match event {
            WorkerEvent::TestA => log::info!("got event - a"),
            WorkerEvent::TestB => log::info!("got event - b"),
        },
        SageThread::default_start,
        SageThread::default_stop,
    )?;
    worker.start();

    let worker = Arc::from(worker);
    let worker_cp = worker.clone();

    let dispatcher = SageThreadControl::new(
        "dispatch",
        move |_t, event: DispatchEvent| match event {
            DispatchEvent::Dispatch => {
                worker.transmit_event(WorkerEvent::TestA);
                worker.transmit_event(WorkerEvent::TestB)
            }
        },
        SageThread::default_start,
        SageThread::default_stop,
    )?;
    dispatcher.start();

    let dispatcher = Arc::from(dispatcher);
    let dispatcher_cp = dispatcher.clone();
    let t3 = SageThreadControl::new(
        "trigger",
        |_t, _event: ()| {},
        |t| {
            let timer_id = t
                .add_periodic_timer(
                    "test",
                    std::time::Duration::from_secs(1),
                    Arc::new(move || dispatcher.transmit_event(DispatchEvent::Dispatch)),
                )
                .unwrap();
            t.start_timer(&timer_id).unwrap();
        },
        SageThread::default_stop,
    )?;
    t3.start();

    signal_handler::wait_for_exit(move || {
        t3.stop();
        dispatcher_cp.stop();
        worker_cp.stop();
    });

    Ok(())
}
