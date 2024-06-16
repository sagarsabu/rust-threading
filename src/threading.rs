use crate::{
    errors::SageError,
    timer::{Timer, TimerCallbackType, TimerCollection, TimerId, TimerType},
};
use std::{
    sync::{self, atomic, mpsc, Arc},
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

pub struct SageHandler<EventType>
where
    EventType: 'static + Send + Sync,
{
    pub name: String,
    tx_channel: mpsc::Sender<ThreadEvent<EventType>>,
    start_barrier: Arc<sync::Barrier>,
    running: Arc<atomic::AtomicBool>,
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

        let mut thread = SageThread::<EventType> {
            name: name.to_string(),
            running: running.clone(),
            start_barrier: start_barrier.clone(),
            tx_channel: tx_channel.clone(),
            timers: TimerCollection::new(),
        };

        let handle = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                thread.start_barrier.wait();
                thread.running.store(true, atomic::Ordering::Relaxed);

                (start_handler_func)(&mut thread);

                thread.process_events(rx_channel, event_handler_func);

                (stop_handler_func)(&mut thread);

                thread.running.store(false, atomic::Ordering::Relaxed);
            })?;

        Ok(Self {
            name: name.to_string(),
            running: running.clone(),
            start_barrier: start_barrier.clone(),
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
        log::info!("terminating handler={}", self.name);

        self.terminate();
    }

    pub fn is_running(&self) -> bool {
        self.running.load(atomic::Ordering::Relaxed)
    }

    pub fn transmit_event(&self, event: EventType) {
        match self.tx_channel.send(ThreadEvent::HandlerEvent(event)) {
            Ok(_) => log::debug!("transmitted event to handler={}", self.name),
            Err(e) => log::error!("failed to transmit event {} to handler={}", e, self.name),
        }
    }

    fn terminate(&self) {
        const MAX_EXIT_ATTEMPTS: u8 = 5;
        if self.is_running() {
            let mut exit_attempt = 0u8;
            while self.tx_channel.send(ThreadEvent::Exit).is_err()
                && exit_attempt <= MAX_EXIT_ATTEMPTS
            {
                exit_attempt += 1;
                log::error!(
                    "failed to send exit event to handler={}. attempt: {}/{}. trying again.",
                    self.name,
                    exit_attempt,
                    MAX_EXIT_ATTEMPTS
                );
            }
        } else {
            log::warn!("cannot stop handler={} that is not running", self.name);
        }
    }
}

impl<EventType> Drop for SageHandler<EventType>
where
    EventType: 'static + Send + Sync,
{
    fn drop(&mut self) {
        log::info!("dropping handler={}", self.name);

        if !self.is_running() {
            log::warn!("dropping handler={} that is not running", self.name);
            return;
        }

        self.stop();

        if let Some(thread_handle) = self.thread_handle.take() {
            match thread_handle.join() {
                Ok(_) => log::info!("joined thread for handler={}", self.name),
                Err(e) => log::error!("failed to join thread for handler={}. {:?}", self.name, e),
            };
        } else {
            log::error!("join handle does not exist for handler={}", self.name);
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
    running: Arc<atomic::AtomicBool>,
    timers: TimerCollection,
}

impl<EventType> SageThread<EventType>
where
    EventType: 'static + Send + Sync,
{
    pub fn default_start(&mut self) {
        log::info!("starting thread");
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
                ThreadEvent::HandlerEvent(handler_event) => event_handler_func(self, handler_event),
                ThreadEvent::TimerEvent { callback } => {
                    (callback)();
                }
                ThreadEvent::Exit => break,
            }
        }

        log::info!("processing events completed");
    }

    pub fn default_stop(&mut self) {
        log::info!("stopping thread");
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
}

impl<EventType> Drop for SageThread<EventType>
where
    EventType: 'static + Send + Sync,
{
    fn drop(&mut self) {
        log::info!("dropping thread");
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
