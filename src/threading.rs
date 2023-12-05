use crate::errors::WorkerError;
use crossbeam_channel::{Receiver, Sender};
use std::{
    sync::{atomic, Arc},
    thread,
};

pub trait ThreadHandler: 'static + Clone + Send + Sync {
    type HandlerEvent: Send + Sync;

    fn starting(&self) {
        log::info!("starting handler");
    }

    fn stopping(&self) {
        log::info!("stopping handler");
    }

    fn process_events(&self, rx_channel: Receiver<ThreadEvent<Self>>) {
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
    T: ThreadHandler,
{
    HandlerEvent(T::HandlerEvent),
    Exit,
}

pub struct SageThread<T>
where
    T: ThreadHandler,
{
    handler_name: String,
    tx_channel: Sender<ThreadEvent<T>>,
    rx_channel: Receiver<ThreadEvent<T>>,
    handler: T,
    join_handle: Option<thread::JoinHandle<()>>,
    running: Arc<atomic::AtomicBool>,
}

impl<T> SageThread<T>
where
    T: ThreadHandler,
{
    pub fn new<StrLike: AsRef<str>>(thread_name: StrLike, handler: T) -> Self {
        log::info!("creating handler: '{}'", thread_name.as_ref());

        let (tx_channel, rx_channel) = crossbeam_channel::unbounded();

        Self {
            handler_name: thread_name.as_ref().to_owned(),
            tx_channel,
            rx_channel,
            handler,
            join_handle: None,
            running: Arc::new(false.into()),
        }
    }

    pub fn start(&mut self) -> Result<(), WorkerError> {
        log::info!("starting handler: '{}'", self.handler_name);

        let rx_channel_copy = self.rx_channel.clone();
        let handler_copy = self.handler.clone();
        let running_copy = self.running.clone();
        let join_handle = thread::Builder::new()
            .name(self.handler_name.clone())
            .spawn(move || {
                running_copy.store(true, atomic::Ordering::Relaxed);
                handler_copy.starting();
                handler_copy.process_events(rx_channel_copy);
                handler_copy.stopping();
                running_copy.store(false, atomic::Ordering::Relaxed);
            })
            .map_err(WorkerError::Io)?;

        self.join_handle = Some(join_handle);

        Ok(())
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
        if self.is_running() {
            self.stop();
        }

        if let Some(handle) = self.join_handle.take() {
            match handle.join() {
                Ok(_) => log::error!("joined thread for handler: '{}'", self.handler_name),
                Err(e) => log::error!(
                    "failed to join thread for handler: '{}'. {:?}",
                    self.handler_name,
                    e
                ),
            };
        }
    }
}
