mod errors;
mod logging;
mod panic_handler;
mod scoped_deadline;
mod signal_handler;
mod threading;
mod timer;

use crate::errors::SageError;
use crate::threading::{SageHandler, SageThread};
use crate::timer::{Timer, TimerType};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

enum DispatchEvent {
    Dispatch,
}

enum WorkerEvent {
    TestA,
    TestB,
}

fn main() -> Result<(), SageError> {
    logging::setup_logger()?;
    panic_handler::register_panic_handler();

    let worker = Arc::new(SageHandler::new(
        "worker",
        |_thread, event: WorkerEvent| match event {
            WorkerEvent::TestA => log::info!("got event - a"),
            WorkerEvent::TestB => log::info!("got event - b"),
        },
        SageThread::default_start,
        SageThread::default_stop,
    )?);
    worker.start();

    let worker_cp = worker.clone();
    let arc_timer_id = Arc::new(AtomicUsize::new(0));
    let arc_timer_id_start_cp = arc_timer_id.clone();
    let arc_timer_id_stop_cp = arc_timer_id.clone();

    let dispatcher = Arc::new(SageHandler::new(
        "dispatch",
        move |_t, event: DispatchEvent| match event {
            DispatchEvent::Dispatch => {
                worker.transmit_event(WorkerEvent::TestA);
                worker.transmit_event(WorkerEvent::TestB)
            }
        },
        move |t| {
            let timer_id = t
                .add_periodic_timer(
                    "test-periodic-timer",
                    std::time::Duration::from_millis(200),
                    || log::info!("timer for dispatcher triggered"),
                )
                .unwrap();
            t.start_timer(&timer_id).unwrap();
            arc_timer_id_start_cp.store(timer_id, Ordering::Relaxed);
        },
        move |t| {
            let timer_id = arc_timer_id_stop_cp.load(Ordering::Relaxed);
            t.stop_timer(&timer_id).unwrap();
        },
    )?);
    dispatcher.start();

    let dispatcher_cp = dispatcher.clone();

    let dispatch_timer = Timer::new(
        "dispatcher",
        std::time::Duration::from_secs(1),
        TimerType::Periodic,
        move || dispatcher.transmit_event(DispatchEvent::Dispatch),
    )?;
    dispatch_timer.start()?;

    signal_handler::wait_for_exit(move || {
        dispatcher_cp.stop();
        worker_cp.stop();
    });

    Ok(())
}
