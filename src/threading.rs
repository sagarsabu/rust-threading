use crate::errors::WorkerError;
use std::{
    sync::{self, atomic, mpsc, Arc},
    thread,
};

pub trait ThreadHandler: 'static + Send + Sync {
    type HandlerEvent: Send;

    fn starting(&self) {
        log::info!("default starting handler");
    }

    fn stopping(&self) {
        log::info!("default stopping handler");
    }

    fn process_events(&self, rx_channel: mpsc::Receiver<ThreadEvent<Self>>) {
        log::info!("processing events started");

        for rx_event in rx_channel.iter() {
            match rx_event {
                ThreadEvent::Exit => break,
                ThreadEvent::HandlerEvent(handler_event) => self.handle_event(handler_event),
            }
        }

        log::info!("processing events completed");
    }

    fn handle_event(&self, event: Self::HandlerEvent);
}

pub enum ThreadEvent<T>
where
    T: ThreadHandler + ?Sized,
{
    HandlerEvent(T::HandlerEvent),
    Exit,
}

pub struct SageThread<T>
where
    T: ThreadHandler,
{
    handler_name: String,
    tx_channel: mpsc::Sender<ThreadEvent<T>>,
    start_barrier: Arc<sync::Barrier>,
    running: Arc<atomic::AtomicBool>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl<T> SageThread<T>
where
    T: ThreadHandler,
{
    pub fn new<StrLike: AsRef<str>>(thread_name: StrLike, handler: T) -> Result<Self, WorkerError> {
        log::info!("creating handler: '{}'", thread_name.as_ref());

        let (tx_channel, rx_channel) = mpsc::channel();

        let running = Arc::new(atomic::AtomicBool::new(false));
        let running_cp = running.clone();

        let start_barrier = Arc::new(sync::Barrier::new(2));
        let start_barrier_cp = start_barrier.clone();

        let join_handle = Some(
            thread::Builder::new()
                .name(thread_name.as_ref().to_owned())
                .spawn(move || {
                    start_barrier_cp.wait();

                    running_cp.store(true, atomic::Ordering::Relaxed);

                    handler.starting();
                    handler.process_events(rx_channel);
                    handler.stopping();

                    running_cp.store(false, atomic::Ordering::Relaxed);
                })
                .map_err(WorkerError::Io)?,
        );

        Ok(Self {
            handler_name: thread_name.as_ref().to_owned(),
            tx_channel,
            join_handle,
            running,
            // Waiting from inside the thread and that start entry
            start_barrier,
        })
    }

    pub fn start(&self) {
        log::info!("dispatching start for handler: '{}'", self.handler_name);
        // Fill up the barrier
        self.start_barrier.wait();
    }

    pub fn stop(&self) {
        log::info!("stopping handler: '{}'", self.handler_name);

        if self.is_running() {
            const MAX_EXIT_ATTEMPTS: u8 = 5;
            let mut exit_attempt = 0u8;
            while self.tx_channel.send(ThreadEvent::Exit).is_err()
                && exit_attempt <= MAX_EXIT_ATTEMPTS
            {
                exit_attempt += 1;
                log::error!(
                    "failed to send exit event to handler: '{}'. attempt: {}/{}. trying again.",
                    self.handler_name,
                    exit_attempt,
                    MAX_EXIT_ATTEMPTS
                );
            }
        } else {
            log::error!(
                "cannot stop handler: '{}' that is not running",
                self.handler_name
            );
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(atomic::Ordering::Relaxed)
    }

    pub fn transmit_event(&self, event: T::HandlerEvent) {
        match self.tx_channel.send(ThreadEvent::HandlerEvent(event)) {
            Ok(_) => log::debug!("transmitted event to handler: '{}'", self.handler_name),
            Err(e) => log::error!(
                "failed to transmit event {} to handler: '{}'",
                e,
                self.handler_name
            ),
        }
    }
}

impl<T> Drop for SageThread<T>
where
    T: ThreadHandler,
{
    fn drop(&mut self) {
        log::info!("dropping handler: '{}'", self.handler_name);

        if self.is_running() {
            self.stop();
        }

        if let Some(join_handle) = self.join_handle.take() {
            match join_handle.join() {
                Ok(_) => log::error!("joined thread for handler: '{}'", self.handler_name),
                Err(e) => log::error!(
                    "failed to join thread for handler: '{}'. {:?}",
                    self.handler_name,
                    e
                ),
            };
        } else {
            log::error!(
                "join handle does not exist for handler: '{}'",
                self.handler_name
            );
        }
    }
}
