use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

use crossbeam_channel as cc;

use crate::timer::{Timer, TimerID, TimerType};

type TimerCollection = HashMap<TimerID, Timer>;

#[derive(Debug)]
pub enum TimerEV {
    AddTimer {
        id: TimerID,
        name: String,
        delta: std::time::Duration,
        timer_type: TimerType,
        timer_tx: cc::Sender<TimerID>,
    },
    RemoveTimer {
        id: TimerID,
    },
    StartTimer {
        id: TimerID,
    },
    StopTimer {
        id: TimerID,
    },
    StopThread,
}

type TimerThreadOnce = (cc::Sender<TimerEV>, Arc<Mutex<Option<JoinHandle<()>>>>);

pub fn get_next_timer_id() -> TimerID {
    static TIMER_ID: AtomicUsize = AtomicUsize::new(0);
    // automatically wraps around
    TIMER_ID.fetch_add(1, Ordering::Relaxed)
}

fn timer_thread() -> Result<TimerThreadOnce, sg_errors::ErrorWrap> {
    let (tx, rx) = cc::unbounded::<TimerEV>();

    let handler = std::thread::Builder::new()
        .name("timers".to_owned())
        .spawn(move || {
            log::info!("timer thread created");

            let mut timers = TimerCollection::new();

            while let Ok(ev) = rx.recv() {
                match ev {
                    TimerEV::AddTimer {
                        id,
                        name,
                        delta,
                        timer_type,
                        timer_tx,
                    } => match Timer::new(
                        &name,
                        delta,
                        timer_type,
                        Box::new(move || {
                            if let Err(e) = timer_tx.send(id) {
                                log::error!("failed to send timer for id {}. {}", id, e);
                            }
                        }),
                    ) {
                        Ok(timer) => {
                            timers.insert(id, timer);
                        }
                        Err(e) => {
                            log::error!("failed to add timer. {}", e);
                        }
                    },
                    TimerEV::RemoveTimer { id } => {
                        if let Some(timer) = timers.remove(&id) {
                            drop(timer);
                        }
                    }
                    TimerEV::StartTimer { id } => {
                        if let Some(timer) = timers.get_mut(&id) {
                            if let Err(e) = timer.start() {
                                log::error!("failed to start timer. {}", e);
                            }
                        }
                    }
                    TimerEV::StopTimer { id } => {
                        if let Some(timer) = timers.get_mut(&id) {
                            if let Err(e) = timer.stop() {
                                log::error!("failed to stop timer. {}", e);
                            }
                        }
                    }
                    TimerEV::StopThread => break,
                }
            }

            drop(rx);

            for timer in timers.values() {
                if let Err(e) = timer.stop() {
                    log::error!(
                        "failed to stop timer {} when stopping timer thread. {}",
                        timer,
                        e
                    );
                }
            }

            drop(timers);

            log::info!("timer thread stopped");
        })?;

    Ok((tx, Arc::new(Mutex::new(Some(handler)))))
}

static ONCE: OnceLock<TimerThreadOnce> = std::sync::OnceLock::new();

pub fn create() {
    ONCE.get_or_init(|| timer_thread().expect("failed to create timer thread"));
}

fn get_timer_thread() -> &'static TimerThreadOnce {
    ONCE.get().expect("timer thread has not been created")
}

pub fn request_timer_event(event: TimerEV) -> Result<(), sg_errors::ErrorWrap> {
    let sender = &get_timer_thread().0;
    sender.send(event).map_err(sg_errors::ErrorWrap::to_generic)
}

pub fn stop() {
    let (sender, handle) = get_timer_thread();
    sender.send(TimerEV::StopThread).unwrap();
    if let Some(handler) = handle.lock().unwrap().take() {
        handler.join().unwrap();
    }
}
