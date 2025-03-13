use crate::{
    scoped_deadline::ScopedDeadline,
    socket_handler::{IoEvent, TcpServerPoller},
    timer::{Timer, TimerID, TimerType},
};
use crossbeam_channel as cc;
use sg_errors::ErrorWrap;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        self,
        atomic::{self},
        Arc,
    },
    thread,
};

type TimerCollection = HashMap<TimerID, Timer>;

enum Event<HandlerEvent>
where
    HandlerEvent: Send,
{
    Handler(HandlerEvent),
}

pub trait Handler {
    type HandlerEvent: Send;

    fn on_start(&mut self, thread: &mut Executor) -> Result<(), ErrorWrap> {
        log::info!("starting thread name={}", thread.name);
        Ok(())
    }

    fn on_handler_event(&mut self, thread: &mut Executor, event: Self::HandlerEvent);

    fn on_io_event(&mut self, _thread: &mut Executor, _io_event: IoEvent) {}

    fn on_stop(&mut self, thread: &mut Executor) -> Result<(), ErrorWrap> {
        log::info!("stopping thread name={}", thread.name);
        Ok(())
    }
}

pub struct Handle<HandlerEvent>
where
    HandlerEvent: 'static + Send,
{
    name: String,
    event_tx: cc::Sender<Event<HandlerEvent>>,
    exit_tx: cc::Sender<()>,
    start_barrier: Arc<sync::Barrier>,
    running: Arc<atomic::AtomicBool>,
    stop_requested: std::cell::Cell<bool>,
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

        let (event_tx, event_rx) = cc::unbounded();
        // zero capacity blocking channel to sync shutdown
        let (exit_tx, exit_rx) = cc::bounded(0);

        let start_barrier = Arc::new(sync::Barrier::new(2));
        let running = Arc::new(atomic::AtomicBool::new(false));

        let name_cp = name.to_string();
        let running_cp = running.clone();
        let start_barrier_cp = start_barrier.clone();

        let handle = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let thread = Executor {
                    name: name_cp,
                    running: running_cp,
                    start_barrier: start_barrier_cp,
                    timers: TimerCollection::new(),
                    io_handles: Vec::new(),
                };
                thread.thread_entry(handler_maker, event_rx, exit_rx);
            })?;

        Ok(Self {
            name: name.to_string(),
            running,
            start_barrier,
            stop_requested: false.into(),
            thread_handle: Some(handle),
            event_tx,
            exit_tx,
        })
    }

    pub fn start(&self) {
        log::info!("dispatching start for handler={}", self.name);
        // Fill up the barrier
        self.start_barrier.wait();
    }

    pub fn stop(&self) {
        if !self.stop_requested.get() {
            log::info!("terminating handler={}", self.name);
            self.terminate();
            self.stop_requested.set(true);
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(atomic::Ordering::SeqCst)
    }

    pub fn transmit_event(&self, event: HandlerEvent) {
        match self.event_tx.send(Event::Handler(event)) {
            Ok(_) => log::debug!("transmitted event to handler={}", self.name),
            Err(e) => log::error!("failed to transmit event {} to handler={}", e, self.name),
        }
    }

    fn terminate(&self) {
        if !self.is_running() {
            return;
        }

        self.exit_tx
            .send(())
            .expect("Failed to send exit event to thread");
    }
}

impl<HandlerEvent> Drop for Handle<HandlerEvent>
where
    HandlerEvent: 'static + Send,
{
    fn drop(&mut self) {
        log::info!("dropping handler={}", self.name);

        self.stop();

        if let Some(thread_handle) = self.thread_handle.take() {
            log::info!("started join on thread for handler={}", self.name);
            match thread_handle.join() {
                Ok(_) => log::info!("joined thread for handler={}", self.name),
                Err(e) => log::error!("failed to join thread for handler={}. {:?}", self.name, e),
            };
        } else {
            log::error!("join handle does not exist for handler={}", self.name);
        }
    }
}

pub struct Executor {
    pub name: String,
    start_barrier: Arc<sync::Barrier>,
    running: Arc<atomic::AtomicBool>,
    timers: TimerCollection,
    io_handles: Vec<TcpServerPoller>,
}

impl Executor {
    fn thread_entry<HandlerMaker, HandlerEvent>(
        self,
        handler_maker: HandlerMaker,
        event_rx: cc::Receiver<Event<HandlerEvent>>,
        exit_rx: cc::Receiver<()>,
    ) where
        HandlerMaker: FnOnce() -> Box<dyn Handler<HandlerEvent = HandlerEvent>>,
        HandlerEvent: Send,
    {
        let mut runner = handler_maker();
        let mut this = self;

        this.start_barrier.wait();
        this.running.store(true, atomic::Ordering::SeqCst);

        if let Err(e) = runner.on_start(&mut this) {
            log::error!("error encountered during start action. {}", e);
        }

        let (mut this, mut runner) = this.process_events(event_rx, exit_rx, runner);

        if let Err(e) = runner.on_stop(&mut this) {
            log::error!("error encountered during stop action. {}", e);
        }

        this.stop_all_timers();

        this.running.store(false, atomic::Ordering::SeqCst);
    }

    fn process_events<HandlerEvent>(
        mut self,
        event_rx: cc::Receiver<Event<HandlerEvent>>,
        exit_rx: cc::Receiver<()>,
        mut runner: Box<dyn Handler<HandlerEvent = HandlerEvent>>,
    ) -> (Self, Box<dyn Handler<HandlerEvent = HandlerEvent>>)
    where
        HandlerEvent: Send,
    {
        const DEADLINE: std::time::Duration = std::time::Duration::from_millis(100);

        log::info!("processing events started for name={}", self.name);

        let mut select = cc::Select::new_biased();
        let exit_select = select.recv(&exit_rx);
        let event_select = select.recv(&event_rx);

        loop {
            match select.select_timeout(std::time::Duration::from_millis(20)) {
                Ok(select_op) => match select_op.index() {
                    // exit handling
                    op if exit_select == op => {
                        select_op.recv(&exit_rx).expect("exit_rx recv failed");
                        log::info!("received exit event. exiting thread.");
                        break;
                    }

                    op if event_select == op => {
                        match select_op.recv(&event_rx).expect("even_rx receive failed") {
                            Event::Handler(handler_event) => {
                                let _dl = ScopedDeadline::new(
                                    format!("handle-event-dl-{}", self.name),
                                    DEADLINE,
                                );
                                runner.on_handler_event(&mut self, handler_event);
                            }
                        }
                    }

                    invalid => panic!("invalid select index {}", invalid),
                },

                // select timeout
                Err(_) => {
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
            }
        }

        log::info!("processing events completed");

        (self, runner)
    }

    // io

    pub fn add_socket_listener(&mut self, socket_address: SocketAddr) -> Result<(), ErrorWrap> {
        self.io_handles.push(TcpServerPoller::new(socket_address)?);

        Ok(())
    }

    // timers

    pub fn add_periodic_timer(
        &mut self,
        name: &str,
        delta: std::time::Duration,
        callback: Box<dyn FnMut()>,
    ) -> Result<TimerID, ErrorWrap> {
        let timer = Timer::new(name, delta, TimerType::Periodic, callback)?;
        let timer_id = timer.get_id();
        self.timers.insert(timer_id, timer);

        Ok(timer_id)
    }

    pub fn start_timer(&self, timer_id: &TimerID) -> Result<(), ErrorWrap> {
        let timer = self.timers.get(timer_id).ok_or_else(|| {
            format!(
                "Cannot start timer with id={} that does not exist",
                timer_id
            )
        })?;

        timer.start()?;
        Ok(())
    }

    pub fn stop_timer(&self, timer_id: &TimerID) -> Result<(), ErrorWrap> {
        let timer = self
            .timers
            .get(timer_id)
            .ok_or_else(|| format!("Cannot stop timer with id={} that does not exist", timer_id))?;

        timer.stop()?;
        Ok(())
    }

    fn stop_all_timers(&self) {
        for (timer_id, timer) in self.timers.iter() {
            if let Err(e) = Timer::stop(timer) {
                log::error!(
                    "failed to stop timer-id={} handler={} {}",
                    timer_id,
                    self.name,
                    e
                );
            } else {
                log::debug!("timer-id={} for handler={} stopped", timer_id, self.name);
            }
        }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        log::info!("dropping thread name={}", self.name);
        // so all io is shutdown before thread exists
        self.io_handles.clear();
    }
}
