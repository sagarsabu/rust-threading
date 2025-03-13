use sg_errors::ErrorWrap;
use sg_threading::{
    timer::{Timer, TimerID, TimerType},
    Executor, Handle, Handler,
};
use std::{sync::Arc, time::Duration};

enum DispatchEvent {
    Dispatch,
}

struct Dispatcher {
    dispatch_timer_id: TimerID,
    dispatch_cntr: i32,
    workers: Vec<Handle<WorkerEvent>>,
}

impl Handler for Dispatcher {
    type HandlerEvent = DispatchEvent;

    fn on_start(&mut self, thread: &mut Executor) -> Result<(), ErrorWrap> {
        for worker in &self.workers {
            worker.start();
        }

        self.dispatch_timer_id = thread.add_periodic_timer(
            "dispatcher-periodic-timer",
            std::time::Duration::from_millis(500),
            Box::new(|| {
                log::info!("timer for dispatcher triggered");
            }),
        )?;

        thread.start_timer(&self.dispatch_timer_id)?;
        let x = thread.add_periodic_timer(
            "dispatch-loopback-timer",
            Duration::from_millis(250),
            Box::new(move || {
                log::info!("sending dispatch event from dispatcher to all workers");
                //                for worker in workers {
                //                    worker.transmit_event(WorkerEvent::TestC);
                //                }
            }),
        )?;
        thread.start_timer(&x)?;

        Ok(())
    }

    fn on_handler_event(&mut self, _thread: &mut Executor, event: Self::HandlerEvent) {
        match event {
            DispatchEvent::Dispatch => {
                let event = if self.dispatch_cntr % 2 == 0 {
                    WorkerEvent::TestA
                } else {
                    WorkerEvent::TestB
                };

                self.dispatch_cntr += 1;
                for worker in &self.workers {
                    worker.transmit_event(event.clone());
                }
            }
        }
    }

    fn on_stop(&mut self, thread: &mut Executor) -> Result<(), ErrorWrap> {
        for worker in &self.workers {
            worker.stop();
        }

        thread.stop_timer(&self.dispatch_timer_id)?;

        Ok(())
    }
}

#[derive(Clone)]
enum WorkerEvent {
    TestA,
    TestB,
    TestC,
}

struct Worker;

impl Handler for Worker {
    type HandlerEvent = WorkerEvent;

    fn on_handler_event(&mut self, _thread: &mut Executor, event: Self::HandlerEvent) {
        match event {
            WorkerEvent::TestA => log::info!("got event - a"),
            WorkerEvent::TestB => log::info!("got event - b"),
            WorkerEvent::TestC => log::info!("got event - c"),
        }
    }
}

fn make_worker_threads(n_workers: usize) -> Result<Vec<Handle<WorkerEvent>>, ErrorWrap> {
    (0..n_workers).try_fold(Vec::new(), |mut acc, idx| {
        let worker = Handle::new(format!("worker-{}", idx + 1), || Box::new(Worker))?;
        acc.push(worker);
        Ok(acc)
    })
}

fn main() -> Result<(), ErrorWrap> {
    sg_logging::setup_logger()?;

    let workers = make_worker_threads(1)?;
    let dispatcher = Arc::new(Handle::new("dispatcher", move || {
        Box::new(Dispatcher {
            dispatch_timer_id: 0,
            dispatch_cntr: 0,
            workers,
        })
    })?);
    dispatcher.start();

    let dispatcher_cp = dispatcher.clone();

    let dispatch_timer = Timer::new(
        "dispatcher",
        std::time::Duration::from_millis(1_000),
        TimerType::Periodic,
        Box::new(move || {
            dispatcher.transmit_event(DispatchEvent::Dispatch);
        }),
    )?;
    dispatch_timer.start()?;

    sg_threading::wait_for_exit(move || {
        dispatch_timer.stop()?;
        dispatcher_cp.stop();

        Ok(())
    });

    Ok(())
}
