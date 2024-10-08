use crate::{
    scoped_deadline::ScopedDeadline,
    socket_handler::{IoEvent, TcpServerPoller},
    timer::{Timer, TimerCallback, TimerCollection, TimerId, TimerType},
};
use sg_errors::ErrorWrap;
use std::{
    net::SocketAddr,
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

pub trait Handler {
    type HandlerEvent: 'static + Send;

    fn on_start(&mut self, thread: &mut Executor<Self::HandlerEvent>) -> Result<(), ErrorWrap> {
        log::info!("starting thread name={}", thread.name);
        Ok(())
    }

    fn on_handler_event(
        &mut self,
        thread: &mut Executor<Self::HandlerEvent>,
        event: Self::HandlerEvent,
    );

    fn on_io_event(&mut self, _thread: &mut Executor<Self::HandlerEvent>, _io_event: IoEvent) {}

    fn on_stop(&mut self, thread: &mut Executor<Self::HandlerEvent>) -> Result<(), ErrorWrap> {
        log::info!("stopping thread name={}", thread.name);
        Ok(())
    }
}

type ExitCV = Arc<(Mutex<bool>, Condvar)>;

pub struct Handle<HandlerEvent>
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

impl<HandlerEvent> Handle<HandlerEvent>
where
    HandlerEvent: 'static + Send,
{
    pub fn new<StrRef: AsRef<str>, HandlerMaker>(
        name: StrRef,
        handler_maker: HandlerMaker,
    ) -> Result<Self, ErrorWrap>
    where
        HandlerMaker: FnOnce() -> Box<dyn Handler<HandlerEvent = HandlerEvent>> + 'static + Send,
    {
        let name = name.as_ref();
        log::info!("creating handler: {}", name);

        let (tx_channel, rx_channel) = mpsc::channel();
        let start_barrier = Arc::new(sync::Barrier::new(2));
        let running = Arc::new(atomic::AtomicBool::new(false));
        let exit_notifier = ExitCV::new((Mutex::new(false), Condvar::new()));

        let thread = Executor::<HandlerEvent> {
            name: name.to_string(),
            running: running.clone(),
            start_barrier: start_barrier.clone(),
            exit_notifier: exit_notifier.clone(),
            tx_channel: tx_channel.clone(),
            timers: TimerCollection::new(),
            io_handles: Vec::new(),
        };

        let handle = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                thread.thread_entry(handler_maker, rx_channel);
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

impl<HandlerEvent> Drop for Handle<HandlerEvent>
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

pub struct Executor<HandlerEvent>
where
    HandlerEvent: 'static + Send,
{
    pub name: String,
    tx_channel: mpsc::Sender<Event<HandlerEvent>>,
    start_barrier: Arc<sync::Barrier>,
    exit_notifier: ExitCV,
    running: Arc<atomic::AtomicBool>,
    timers: TimerCollection,
    io_handles: Vec<TcpServerPoller>,
}

impl<HandlerEvent> Executor<HandlerEvent>
where
    HandlerEvent: 'static + Send,
{
    fn thread_entry<HandlerMaker>(
        self,
        handler_maker: HandlerMaker,
        rx_channel: mpsc::Receiver<Event<HandlerEvent>>,
    ) where
        HandlerMaker: FnOnce() -> Box<dyn Handler<HandlerEvent = HandlerEvent>>,
    {
        let mut runner = handler_maker();
        let mut this_executor = self;

        this_executor.start_barrier.wait();
        this_executor.running.store(true, atomic::Ordering::Release);

        if let Err(e) = runner.on_start(&mut this_executor) {
            log::error!("error encountered during start action. {}", e);
        }

        let (mut this_executor, mut runner) = this_executor.process_events(rx_channel, runner);

        if let Err(e) = runner.on_stop(&mut this_executor) {
            log::error!("error encountered during stop action. {}", e);
        }

        this_executor.stop_all_timers();

        this_executor
            .running
            .store(false, atomic::Ordering::Release);
    }

    fn process_events(
        mut self,
        rx_channel: mpsc::Receiver<Event<HandlerEvent>>,
        mut runner: Box<dyn Handler<HandlerEvent = HandlerEvent>>,
    ) -> (Self, Box<dyn Handler<HandlerEvent = HandlerEvent>>) {
        const DEADLINE: std::time::Duration = std::time::Duration::from_millis(100);

        log::info!("processing events started for name={}", self.name);

        loop {
            match rx_channel.recv_timeout(std::time::Duration::from_millis(20)) {
                Ok(rx_event) => {
                    match rx_event {
                        Event::Handler(handler_event) => {
                            let _dl = ScopedDeadline::new(
                                format!("handle-event-dl-{}", self.name),
                                DEADLINE,
                            );
                            runner.on_handler_event(&mut self, handler_event);
                        }

                        Event::Timer { callback } => {
                            let _dl = ScopedDeadline::new(
                                format!("timer-event-dl-{}", self.name),
                                DEADLINE,
                            );
                            (callback)();
                        }

                        // TODO Need a way to make high priority events
                        Event::Exit => {
                            log::info!(
                                "received exit event. notifying handler and exiting thread."
                            );
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

                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Handle io polling
                    let mut all_read_events = vec![];
                    for io_handle in &mut self.io_handles {
                        match io_handle.poll_events() {
                            Ok(mut read_events) => {
                                all_read_events.append(&mut read_events);
                            }
                            Err(e) => log::error!("io handler poll error. {}", e),
                        }
                    }

                    for read_event in all_read_events {
                        let _dl = ScopedDeadline::new(
                            format!("io-read-event-dl-{}", self.name),
                            DEADLINE,
                        );
                        runner.on_io_event(&mut self, read_event);
                    }
                }

                // Handler dropped or shutdown
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        log::info!("processing events completed");

        (self, runner)
    }

    // io

    pub fn add_socket_listener(&mut self, socket_address: SocketAddr) -> Result<(), ErrorWrap> {
        self.io_handles
            .push(TcpServerPoller::new(socket_address)?);

        Ok(())
    }

    // timers

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

impl<EventType> Drop for Executor<EventType>
where
    EventType: 'static + Send,
{
    fn drop(&mut self) {
        log::info!("dropping thread name={}", self.name);
        // so all io is shutdown before thread exists
        self.io_handles.clear();
    }
}
