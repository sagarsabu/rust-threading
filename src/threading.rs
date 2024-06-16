use crate::{
    errors::SageError,
    scoped_deadline::ScopedDeadline,
    timer::{Timer, TimerCallbackType, TimerCollection, TimerId, TimerType},
};
use std::{
    sync::{
        self,
        atomic::{self, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
};

enum ThreadEvent<EventType>
where
    EventType: 'static + Send + Sync,
{
    HandlerEvent(EventType),
    TimerEvent { callback: TimerCallbackType },
    Exit,
}

type ExitCV = (Mutex<bool>, Condvar);

pub struct SageHandler<EventType>
where
    EventType: 'static + Send + Sync,
{
    pub name: String,
    tx_channel: mpsc::Sender<ThreadEvent<EventType>>,
    start_barrier: Arc<sync::Barrier>,
    running: Arc<atomic::AtomicBool>,
    exit_notifier: Arc<ExitCV>,
    stop_requested: Arc<atomic::AtomicBool>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl<EventType> SageHandler<EventType>
where
    EventType: 'static + Send + Sync,
{
    pub fn new<EventHandlerFunc, StartHandlerFunc, StopHandlerFunc>(
        name: &str,
        event_handler_func: EventHandlerFunc,
        start_handler_func: StartHandlerFunc,
        stop_handler_func: StopHandlerFunc,
    ) -> Result<Self, SageError>
    where
        EventHandlerFunc: Fn(&mut SageThread<EventType>, EventType) + 'static + Send,
        StartHandlerFunc: FnOnce(&mut SageThread<EventType>) + 'static + Send,
        StopHandlerFunc: FnOnce(&mut SageThread<EventType>) + 'static + Send,
    {
        log::info!("creating handler: {}", name);

        let (tx_channel, rx_channel) = mpsc::channel();
        let start_barrier = Arc::new(sync::Barrier::new(2));
        let running = Arc::new(atomic::AtomicBool::new(false));
        let exit_notifier = Arc::new((Mutex::new(false), Condvar::new()));

        let thread = SageThread::<EventType> {
            name: name.to_string(),
            running: running.clone(),
            start_barrier: start_barrier.clone(),
            exit_notifier: exit_notifier.clone(),
            tx_channel: tx_channel.clone(),
            timers: TimerCollection::new(),
        };

        let handle = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                thread.thread_entry(
                    rx_channel,
                    event_handler_func,
                    start_handler_func,
                    stop_handler_func,
                );
            })?;

        Ok(Self {
            name: name.to_string(),
            running,
            start_barrier,
            exit_notifier,
            stop_requested: Arc::new(false.into()),
            thread_handle: Some(handle),
            tx_channel,
        })
    }

    pub fn start(&self) {
        log::info!("dispatching start for handler={}", self.name);
        // Fill up the barrier
        self.start_barrier.wait();
    }

    pub fn stop(&self) {
        if !self.stop_requested.load(Ordering::Relaxed) {
            log::info!("terminating handler={}", self.name);
            self.terminate();
            self.stop_requested.store(true, Ordering::Relaxed);
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(atomic::Ordering::Acquire)
    }

    pub fn transmit_event(&self, event: EventType) {
        match self.tx_channel.send(ThreadEvent::HandlerEvent(event)) {
            Ok(_) => log::debug!("transmitted event to handler={}", self.name),
            Err(e) => log::error!("failed to transmit event {} to handler={}", e, self.name),
        }
    }

    fn terminate(&self) {
        const MAX_EXIT_ATTEMPTS: u8 = 5;
        let mut exit_attempt = 0u8;

        while exit_attempt <= MAX_EXIT_ATTEMPTS {
            if !self.is_running() {
                break;
            }

            if self.tx_channel.send(ThreadEvent::Exit).is_err() {
                exit_attempt += 1;
                log::warn!(
                    "failed to send exit event to handler={}. attempt: {}/{}. trying again.",
                    self.name,
                    exit_attempt,
                    MAX_EXIT_ATTEMPTS
                );
            } else {
                break;
            }
        }
    }
}

impl<EventType> Drop for SageHandler<EventType>
where
    EventType: 'static + Send + Sync,
{
    fn drop(&mut self) {
        const DEADLINE: std::time::Duration = std::time::Duration::from_millis(100);

        log::info!("dropping handler={}", self.name);

        self.stop();

        let (lock, cv) = &(*self.exit_notifier);

        if let Ok(lock_guard) = lock.lock() {
            match cv.wait_timeout_while(lock_guard, DEADLINE, |exit_acknowledged| {
                !*exit_acknowledged
            }) {
                Ok((_lock_guard, wait_res)) => {
                    if wait_res.timed_out() {
                        log::error!(
                            "thread handler={} was not notified of exit within deadline={:?}",
                            self.name,
                            DEADLINE
                        );
                        return;
                    }

                    if let Some(thread_handle) = self.thread_handle.take() {
                        match thread_handle.join() {
                            Ok(_) => log::info!("joined thread for handler={}", self.name),
                            Err(e) => log::error!(
                                "failed to join thread for handler={}. {:?}",
                                self.name,
                                e
                            ),
                        };
                    } else {
                        log::error!("join handle does not exist for handler={}", self.name);
                    }
                }

                Err(e) => log::error!(
                    "failed to lock exit cv when waiting in exit cv for handler={} {}",
                    self.name,
                    e
                ),
            }
        } else {
            log::error!("failed to lock exit cv when dropping handler={}", self.name,)
        }
    }
}

pub struct SageThread<EventType>
where
    EventType: 'static + Send + Sync,
{
    name: String,
    tx_channel: mpsc::Sender<ThreadEvent<EventType>>,
    start_barrier: Arc<sync::Barrier>,
    exit_notifier: Arc<ExitCV>,
    running: Arc<atomic::AtomicBool>,
    timers: TimerCollection,
}

impl<EventType> SageThread<EventType>
where
    EventType: 'static + Send + Sync,
{
    pub fn default_start(&mut self) {
        log::info!("starting thread name={}", self.name);
    }

    fn thread_entry<EventHandlerFunc, StartHandlerFunc, StopHandlerFunc>(
        mut self,
        rx_channel: mpsc::Receiver<ThreadEvent<EventType>>,
        event_handler_func: EventHandlerFunc,
        start_handler_func: StartHandlerFunc,
        stop_handler_func: StopHandlerFunc,
    ) where
        EventHandlerFunc: Fn(&mut Self, EventType) + 'static + Send,
        StartHandlerFunc: FnOnce(&mut Self) + 'static + Send,
        StopHandlerFunc: FnOnce(&mut Self) + 'static + Send,
    {
        self.start_barrier.wait();
        self.running.store(true, atomic::Ordering::Release);

        (start_handler_func)(&mut self);

        self.process_events(rx_channel, event_handler_func);

        (stop_handler_func)(&mut self);

        self.stop_all_timers();

        self.running.store(false, atomic::Ordering::Release);
    }

    fn process_events<EventHandlerFunc>(
        &mut self,
        rx_channel: mpsc::Receiver<ThreadEvent<EventType>>,
        event_handler_func: EventHandlerFunc,
    ) where
        EventHandlerFunc: Fn(&mut Self, EventType) + 'static + Send,
    {
        log::info!("processing events started for name={}", self.name);

        for rx_event in rx_channel.iter() {
            match rx_event {
                ThreadEvent::HandlerEvent(handler_event) => {
                    let _dl = ScopedDeadline::new(
                        format!("handle-event-dl-{}", self.name),
                        std::time::Duration::from_millis(100),
                    );
                    event_handler_func(self, handler_event);
                }
                ThreadEvent::TimerEvent { callback } => {
                    let _dl = ScopedDeadline::new(
                        format!("timer-event-dl-{}", self.name),
                        std::time::Duration::from_millis(100),
                    );
                    (callback)();
                }
                ThreadEvent::Exit => {
                    log::info!("received exit event. notifying handler and exiting thread.");
                    let (lock, cv) = &*self.exit_notifier;
                    if let Ok(mut lock_guard) = lock.lock() {
                        *lock_guard = true;
                        cv.notify_one();
                    } else {
                        log::error!("failed to lock exit cv on exit event");
                    }
                    break;
                }
            }
        }

        log::info!("processing events completed");
    }

    pub fn default_stop(&mut self) {
        log::info!("stopping thread name={}", self.name);
    }

    pub fn add_periodic_timer<F: Fn() + 'static + Send + Sync>(
        &mut self,
        name: &str,
        delta: std::time::Duration,
        callback: F,
    ) -> Result<TimerId, SageError> {
        let tx_channel_cp = self.tx_channel.clone();
        let cb: Arc<dyn Fn() + 'static + Send + Sync> = Arc::new(callback);
        let name_cp = self.name.clone();
        let timer = Timer::new(name, delta, TimerType::Periodic, move || {
            let event = ThreadEvent::<EventType>::TimerEvent {
                callback: cb.clone(),
            };
            match tx_channel_cp.send(event) {
                Ok(_) => log::debug!("transmitted timer event to handler={}", name_cp),
                Err(e) => log::error!("{}", e),
            }
        })?;

        let timer_id = timer.get_id();
        self.timers.insert(timer_id, timer);

        Ok(timer_id)
    }

    pub fn start_timer(&self, timer_id: &TimerId) -> Result<(), SageError> {
        let timer = self.timers.get(timer_id).ok_or_else(|| {
            format!(
                "Cannot start timer with id={} that does not exist",
                timer_id
            )
        })?;

        timer.start()?;
        Ok(())
    }

    pub fn stop_timer(&self, timer_id: &TimerId) -> Result<(), SageError> {
        let timer = self
            .timers
            .get(timer_id)
            .ok_or_else(|| format!("Cannot stop timer with id={} that does not exist", timer_id))?;

        timer.stop()?;
        Ok(())
    }

    fn stop_all_timers(&self) {
        for (timer_id, timer) in self.timers.iter() {
            match Timer::stop(timer) {
                Ok(_) => log::debug!("timer-id={} for handler={} stopped", timer_id, self.name),
                Err(e) => log::error!(
                    "failed to stop timer-id={} handler={} {}",
                    timer_id,
                    self.name,
                    e
                ),
            }
        }
    }
}

impl<EventType> Drop for SageThread<EventType>
where
    EventType: 'static + Send + Sync,
{
    fn drop(&mut self) {
        log::info!("dropping thread name={}", self.name);
    }
}
