use crate::{
    scoped_deadline::ScopedDeadline,
    socket_handler::{IoEvent, TcpServerPoller},
    timer::{TimerID, TimerType},
    time_handler::{self, TimerEV},
};
use crossbeam_channel as cc;
use sg_errors::ErrorWrap;
use std::{
    net::SocketAddr,
    sync::{
        self, Arc,
        atomic::{self},
    },
    thread,
};

enum Event<HandlerEvent> {
    Handler(HandlerEvent),
}

pub trait Handler<HandlerEvent> {
    fn on_start(&mut self, thread: &mut Executor) -> Result<(), ErrorWrap> {
        log::info!("starting thread name={}", thread.name);
        Ok(())
    }

    #[allow(unused_variables)]
    fn on_handler_event(
        &mut self,
        thread: &mut Executor,
        event: HandlerEvent,
    ) -> Result<(), ErrorWrap> {
        Ok(())
    }

    #[allow(unused_variables)]
    fn on_timer_event(&mut self, thread: &mut Executor, id: TimerID) -> Result<(), ErrorWrap> {
        Ok(())
    }

    #[allow(unused_variables)]
    fn on_io_event(&mut self, thread: &mut Executor, io_event: IoEvent) -> Result<(), ErrorWrap> {
        Ok(())
    }

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
        HandlerMaker: FnOnce() -> Box<dyn Handler<HandlerEvent>> + 'static + Send,
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
                let (timer_tx, timer_rx) = cc::unbounded();
                let thread = Executor {
                    name: name_cp,
                    running: running_cp,
                    start_barrier: start_barrier_cp,
                    timer_tx,
                    io_handles: Vec::new(),
                };
                thread.thread_entry(handler_maker, event_rx, exit_rx, timer_rx);
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
    timer_tx: cc::Sender<TimerID>,
    io_handles: Vec<TcpServerPoller>,
}

impl Executor {
    fn thread_entry<HandlerMaker, HandlerEvent>(
        self,
        handler_maker: HandlerMaker,
        event_rx: cc::Receiver<Event<HandlerEvent>>,
        exit_rx: cc::Receiver<()>,
        timer_rx: cc::Receiver<TimerID>,
    ) where
        HandlerMaker: FnOnce() -> Box<dyn Handler<HandlerEvent>>,
        HandlerEvent: Send,
    {
        let mut runner = handler_maker();
        let mut this = self;

        this.start_barrier.wait();
        this.running.store(true, atomic::Ordering::SeqCst);

        if let Err(e) = runner.on_start(&mut this) {
            log::error!("error encountered during start action. {}", e);
        }

        let (mut this, mut runner) = this.process_events(event_rx, exit_rx, timer_rx, runner);

        if let Err(e) = runner.on_stop(&mut this) {
            log::error!("error encountered during stop action. {}", e);
        }

        // so all io is shutdown before thread exists
        this.io_handles.clear();

        this.running.store(false, atomic::Ordering::SeqCst);
    }

    fn process_events<HandlerEvent>(
        mut self,
        event_rx: cc::Receiver<Event<HandlerEvent>>,
        exit_rx: cc::Receiver<()>,
        timer_rx: cc::Receiver<TimerID>,
        mut runner: Box<dyn Handler<HandlerEvent>>,
    ) -> (Self, Box<dyn Handler<HandlerEvent>>)
    where
        HandlerEvent: Send,
    {
        const DEADLINE: std::time::Duration = std::time::Duration::from_millis(100);

        log::info!("processing events started for name={}", self.name);
        let mut select = cc::Select::new_biased();
        let exit_select = select.recv(&exit_rx);
        let event_select = select.recv(&event_rx);
        let timer_select = select.recv(&timer_rx);

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
                                if let Err(e) = runner.on_handler_event(&mut self, handler_event) {
                                    log::warn!("{} on_handler_event {}", self.name, e);
                                }
                            }
                        }
                    }

                    op if timer_select == op => {
                        let timer_id = select_op.recv(&timer_rx).expect("timer_rx receive failed");
                        let _dl =
                            ScopedDeadline::new(format!("timer-event-dl-{}", self.name), DEADLINE);
                        if let Err(e) = runner.on_timer_event(&mut self, timer_id) {
                            log::warn!("{} on_timer_event {}", self.name, e);
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
                        if let Err(e) = runner.on_io_event(&mut self, read_event) {
                            log::warn!("{} on_io_event {}", self.name, e);
                        }
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
        &self,
        name: &str,
        delta: std::time::Duration,
    ) -> Result<TimerID, ErrorWrap> {
        let id = time_handler::get_next_timer_id();
        time_handler::request_timer_event(TimerEV::AddTimer {
            id,
            name: name.to_owned(),
            delta,
            timer_type: TimerType::Periodic,
            timer_tx: self.timer_tx.clone(),
        })?;

        Ok(id)
    }

    pub fn start_timer(&self, id: TimerID) -> Result<(), ErrorWrap> {
        time_handler::request_timer_event(TimerEV::StartTimer { id })
    }

    pub fn stop_timer(&self, id: TimerID) -> Result<(), ErrorWrap> {
        time_handler::request_timer_event(TimerEV::StopTimer { id })
    }
}
