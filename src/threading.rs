use crate::{
    errors::ErrorWrap,
    scoped_deadline::ScopedDeadline,
    timer::{Timer, TimerCallback, TimerCollection, TimerId, TimerType},
};
use std::{
    sync::{
        self,
        atomic::{self, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
};

enum Event<HandlerEvent>
where
    HandlerEvent: 'static + Send,
{
    Handler(HandlerEvent),
    Timer { callback: TimerCallback },
    Exit,
}

type ExitCV = Arc<(Mutex<bool>, Condvar)>;

pub struct ThreadHandler<HandlerEvent>
where
    HandlerEvent: 'static + Send,
{
    name: String,
    tx_channel: mpsc::Sender<Event<HandlerEvent>>,
    start_barrier: Arc<sync::Barrier>,
    running: Arc<atomic::AtomicBool>,
    exit_notifier: ExitCV,
    stop_requested: Arc<atomic::AtomicBool>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl<HandlerEvent> ThreadHandler<HandlerEvent>
where
    HandlerEvent: 'static + Send,
{
    pub fn new<ThreadData, ThreadDataMaker, EventHandler, StartHandler, StopHandler>(
        name: &str,
        data_maker: ThreadDataMaker,
        event_handler: EventHandler,
        start_handler: StartHandler,
        stop_handler: StopHandler,
    ) -> Result<Self, ErrorWrap>
    where
        ThreadData: 'static + Send,
        ThreadDataMaker: FnOnce() -> ThreadData,
        EventHandler:
            Fn(&mut ThreadExecutor<ThreadData, HandlerEvent>, HandlerEvent) + 'static + Send,
        StartHandler: FnOnce(&mut ThreadExecutor<ThreadData, HandlerEvent>) -> Result<(), ErrorWrap>
            + 'static
            + Send,
        StopHandler: FnOnce(&mut ThreadExecutor<ThreadData, HandlerEvent>) -> Result<(), ErrorWrap>
            + 'static
            + Send,
    {
        log::info!("creating handler: {}", name);

        let (tx_channel, rx_channel) = mpsc::channel();
        let start_barrier = Arc::new(sync::Barrier::new(2));
        let running = Arc::new(atomic::AtomicBool::new(false));
        let exit_notifier = ExitCV::new((Mutex::new(false), Condvar::new()));

        let thread = ThreadExecutor::<ThreadData, HandlerEvent> {
            name: name.to_string(),
            data: data_maker(),
            running: running.clone(),
            start_barrier: start_barrier.clone(),
            exit_notifier: exit_notifier.clone(),
            tx_channel: tx_channel.clone(),
            timers: TimerCollection::new(),
        };

        let handle = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                thread.thread_entry(rx_channel, event_handler, start_handler, stop_handler);
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

    pub fn transmit_event(&self, event: HandlerEvent) {
        match self.tx_channel.send(Event::Handler(event)) {
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

            if self.tx_channel.send(Event::Exit).is_err() {
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

impl<HandlerEvent> Drop for ThreadHandler<HandlerEvent>
where
    HandlerEvent: 'static + Send,
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

pub struct ThreadExecutor<Data, HandlerEvent>
where
    Data: 'static + Send,
    HandlerEvent: 'static + Send,
{
    pub name: String,
    pub data: Data,
    tx_channel: mpsc::Sender<Event<HandlerEvent>>,
    start_barrier: Arc<sync::Barrier>,
    exit_notifier: ExitCV,
    running: Arc<atomic::AtomicBool>,
    timers: TimerCollection,
}

impl<Data, HandlerEvent> ThreadExecutor<Data, HandlerEvent>
where
    Data: 'static + Send,
    HandlerEvent: 'static + Send,
{
    pub fn default_start(&mut self) -> Result<(), ErrorWrap> {
        log::info!("starting thread name={}", self.name);
        Ok(())
    }

    fn thread_entry<EventHandler, StartHandler, StopHandler>(
        mut self,
        rx_channel: mpsc::Receiver<Event<HandlerEvent>>,
        event_handler: EventHandler,
        start_handler: StartHandler,
        stop_handler: StopHandler,
    ) where
        EventHandler: Fn(&mut Self, HandlerEvent) + 'static + Send,
        StartHandler: FnOnce(&mut Self) -> Result<(), ErrorWrap> + 'static + Send,
        StopHandler: FnOnce(&mut Self) -> Result<(), ErrorWrap> + 'static + Send,
    {
        self.start_barrier.wait();
        self.running.store(true, atomic::Ordering::Release);

        if let Err(e) = (start_handler)(&mut self) {
            log::error!("error encountered during start action. {}", e);
        }

        self.process_events(rx_channel, event_handler);

        if let Err(e) = (stop_handler)(&mut self) {
            log::error!("error encountered during stop action. {}", e);
        }

        self.stop_all_timers();

        self.running.store(false, atomic::Ordering::Release);
    }

    fn process_events<EventHandler>(
        &mut self,
        rx_channel: mpsc::Receiver<Event<HandlerEvent>>,
        event_handler: EventHandler,
    ) where
        EventHandler: Fn(&mut Self, HandlerEvent) + 'static + Send,
    {
        const DEADLINE: std::time::Duration = std::time::Duration::from_millis(100);

        log::info!("processing events started for name={}", self.name);

        for rx_event in rx_channel.iter() {
            match rx_event {
                Event::Handler(handler_event) => {
                    let _dl =
                        ScopedDeadline::new(format!("handle-event-dl-{}", self.name), DEADLINE);
                    event_handler(self, handler_event);
                }
                Event::Timer { callback } => {
                    let _dl =
                        ScopedDeadline::new(format!("timer-event-dl-{}", self.name), DEADLINE);
                    (callback)();
                }
                // TODO Need a way to make high priority events
                Event::Exit => {
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

    pub fn default_stop(&mut self) -> Result<(), ErrorWrap> {
        log::info!("stopping thread name={}", self.name);
        Ok(())
    }

    pub fn add_periodic_timer<F: Fn() + 'static + Send + Sync>(
        &mut self,
        name: &str,
        delta: std::time::Duration,
        callback: F,
    ) -> Result<TimerId, ErrorWrap> {
        let tx_channel_cp = self.tx_channel.clone();
        let cb: Arc<dyn Fn() + 'static + Send + Sync> = Arc::new(callback);
        let name_cp = self.name.clone();
        let timer = Timer::new(name, delta, TimerType::Periodic, move || {
            let event = Event::<HandlerEvent>::Timer {
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

    pub fn start_timer(&self, timer_id: &TimerId) -> Result<(), ErrorWrap> {
        let timer = self.timers.get(timer_id).ok_or_else(|| {
            format!(
                "Cannot start timer with id={} that does not exist",
                timer_id
            )
        })?;

        timer.start()?;
        Ok(())
    }

    pub fn stop_timer(&self, timer_id: &TimerId) -> Result<(), ErrorWrap> {
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

impl<Data, EventType> Drop for ThreadExecutor<Data, EventType>
where
    Data: 'static + Send,
    EventType: 'static + Send,
{
    fn drop(&mut self) {
        log::info!("dropping thread name={}", self.name);
    }
}
