mod errors;
mod logging;
mod panic_handler;
mod scoped_deadline;
mod signal_handler;
mod threading;
mod timer;

use crate::{
    errors::ErrorWrap,
    threading::{ThreadExecutor, ThreadHandler},
    timer::{Timer, TimerId, TimerType},
};
use std::sync::Arc;

enum DispatchEvent {
    Dispatch,
}

struct DispatchHandlerData {
    dispatch_timer_id: TimerId,
    workers: Vec<ThreadHandler<WorkerEvent>>,
}

enum WorkerEvent {
    TestA,
    TestB,
}

fn make_worker_threads(n_workers: usize) -> Result<Vec<ThreadHandler<WorkerEvent>>, ErrorWrap> {
    (0..n_workers).try_fold(Vec::new(), |mut acc, idx| {
        let worker = ThreadHandler::new(
            format!("worker-{}", idx + 1).as_str(),
            || {},
            |_thread, event: WorkerEvent| match event {
                WorkerEvent::TestA => log::info!("got event - a"),
                WorkerEvent::TestB => log::info!("got event - b"),
            },
            ThreadExecutor::default_start,
            ThreadExecutor::default_stop,
        )?;
        acc.push(worker);
        Ok(acc)
    })
}

fn main() -> Result<(), ErrorWrap> {
    logging::setup_logger()?;
    panic_handler::register_panic_handler();

    let workers = make_worker_threads(4)?;
    let dispatcher = Arc::new(ThreadHandler::new(
        "dispatcher",
        move || DispatchHandlerData {
            dispatch_timer_id: 0,
            workers,
        },
        move |t, event: DispatchEvent| match event {
            DispatchEvent::Dispatch => {
                for worker in t.data.workers.iter() {
                    worker.transmit_event(WorkerEvent::TestA);
                    worker.transmit_event(WorkerEvent::TestB);
                }
            }
        },
        move |t| {
            for worker in &t.data.workers {
                worker.start();
            }

            t.data.dispatch_timer_id = t.add_periodic_timer(
                "test-periodic-timer",
                std::time::Duration::from_millis(500),
                || log::info!("timer for dispatcher triggered"),
            )?;

            t.start_timer(&t.data.dispatch_timer_id)?;

            Ok(())
        },
        move |t| {
            for worker in &t.data.workers {
                worker.stop();
            }

            t.stop_timer(&t.data.dispatch_timer_id)?;

            Ok(())
        },
    )?);
    dispatcher.start();

    let dispatcher_cp = dispatcher.clone();

    let dispatch_timer = Timer::new(
        "dispatcher",
        std::time::Duration::from_millis(1_000),
        TimerType::Periodic,
        move || dispatcher.transmit_event(DispatchEvent::Dispatch),
    )?;
    dispatch_timer.start()?;

    signal_handler::wait_for_exit(move || {
        dispatch_timer.stop()?;
        dispatcher_cp.stop();

        Ok(())
    });

    Ok(())
}
