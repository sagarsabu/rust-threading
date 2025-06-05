use sg_errors::ErrorWrap;
use sg_threading::{
    Executor, Handle, Handler,
    timer::{Timer, TimerID, TimerType},
};
use std::{rc::Rc, time::Duration};

enum DispatchEvent {
    Dispatch,
}

struct Dispatcher {
    dispatch_timer_id: TimerID,
    dispatch_cntr: i32,
    loopback_timer_id: TimerID,
    workers: Vec<Handle<WorkerEvent>>,
}

impl Handler<DispatchEvent> for Dispatcher {
    fn on_start(&mut self, thread: &mut Executor) -> Result<(), ErrorWrap> {
        for worker in &self.workers {
            worker.start();
        }

        self.dispatch_timer_id = thread.add_periodic_timer(
            "dispatcher-periodic-timer",
            std::time::Duration::from_millis(500),
        )?;

        thread.start_timer(self.dispatch_timer_id)?;
        self.loopback_timer_id =
            thread.add_periodic_timer("dispatch-loopback-timer", Duration::from_millis(200))?;
        thread.start_timer(self.loopback_timer_id)?;

        Ok(())
    }

    fn on_handler_event(
        &mut self,
        _thread: &mut Executor,
        event: DispatchEvent,
    ) -> Result<(), ErrorWrap> {
        match event {
            DispatchEvent::Dispatch => {
                let event = match self.dispatch_cntr % 3 {
                    0 => WorkerEvent::TestA,
                    1 => WorkerEvent::TestB,
                    2 => WorkerEvent::TestC,
                    _ => unreachable!(),
                };

                self.dispatch_cntr += 1;
                for worker in &self.workers {
                    worker.transmit_event(event.clone());
                }
            }
        }

        Ok(())
    }

    fn on_timer_event(&mut self, _thread: &mut Executor, id: TimerID) -> Result<(), ErrorWrap> {
        match id {
            id if id == self.dispatch_timer_id => {
                log::info!("timer for dispatcher triggered");
            }
            id if id == self.loopback_timer_id => {
                log::info!("timer for loopback triggered");
                let event = match self.dispatch_cntr % 3 {
                    0 => WorkerEvent::TestA,
                    1 => WorkerEvent::TestB,
                    2 => WorkerEvent::TestC,
                    _ => unreachable!(),
                };
                for worker in &self.workers {
                    worker.transmit_event(event.clone());
                }
            }
            unexpected => {
                log::error!("unexpected timer {} triggered", unexpected);
            }
        };

        Ok(())
    }

    fn on_stop(&mut self, thread: &mut Executor) -> Result<(), ErrorWrap> {
        for worker in &self.workers {
            worker.stop();
        }

        thread.stop_timer(self.dispatch_timer_id)?;

        Ok(())
    }
}

#[allow(dead_code)]
#[derive(Clone)]
enum WorkerEvent {
    TestA,
    TestB,
    TestC,
}

struct Worker;

impl Handler<WorkerEvent> for Worker {
    fn on_handler_event(
        &mut self,
        _thread: &mut Executor,
        event: WorkerEvent,
    ) -> Result<(), ErrorWrap> {
        match event {
            WorkerEvent::TestA => log::info!("got event - a"),
            WorkerEvent::TestB => log::info!("got event - b"),
            WorkerEvent::TestC => log::info!("got event - c"),
        }

        Ok(())
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

    sg_threading::time_handler::create();

    let workers = make_worker_threads(1)?;
    let dispatcher = Rc::new(Handle::new("dispatcher", move || {
        Box::new(Dispatcher {
            dispatch_timer_id: 0,
            dispatch_cntr: 0,
            workers,
            loopback_timer_id: 0,
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

    sg_threading::time_handler::stop();

    Ok(())
}
