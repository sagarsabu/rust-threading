use crate::errors::WorkerError;
use crossbeam_channel::{Receiver, Sender};
use std::thread;

pub trait ThreadHandler {
    type HandlerEvent: Send + Sync;

    fn starting();

    fn stopping();

    fn handle_event(event: Self::HandlerEvent);
}

enum ThreadEvent<T>
where
    T: ThreadHandler,
{
    HandlerEvent(T::HandlerEvent),
    Exit,
}

pub struct SageThread<T>
where
    T: ThreadHandler + 'static,
{
    thread_name: String,
    tx_channel: Sender<ThreadEvent<T>>,
    rx_channel: Receiver<ThreadEvent<T>>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl<T> SageThread<T>
where
    T: ThreadHandler + 'static,
{
    pub fn new<StrLike: AsRef<str>>(thread_name: StrLike) -> Self {
        log::info!("creating thread {}", thread_name.as_ref());

        let (tx_channel, rx_channel) = crossbeam_channel::unbounded();

        Self {
            thread_name: thread_name.as_ref().to_owned(),
            tx_channel,
            rx_channel,
            thread_handle: None,
        }
    }

    pub fn start(&mut self) -> Result<(), WorkerError> {
        T::starting();
        let rx_channel_copy = self.rx_channel.clone();
        self.thread_handle = Some(
            thread::Builder::new()
                .name(self.thread_name.clone())
                .spawn(move || Self::process_events(rx_channel_copy))
                .map_err(WorkerError::Io)?,
        );

        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), WorkerError> {
        T::stopping();
        if let Some(handle) = self.thread_handle.take() {
            let mut exit_attempt = 0u8;
            while self.tx_channel.send(ThreadEvent::Exit).is_err() {
                exit_attempt += 1;
                log::error!(
                    "failed to send exit event for attempt: {}. trying again.",
                    exit_attempt
                );
            }

            handle
                .join()
                .map_err(|e| WorkerError::Any(format!("{e:?}")))?;
        } else {
            log::error!("cannot stop worker that is not running");
        }

        Ok(())
    }

    pub fn transmit_event(&self, event: T::HandlerEvent) {
        match self.tx_channel.send(ThreadEvent::HandlerEvent(event)) {
            Ok(_) => log::debug!("transmitted event"),
            Err(e) => log::error!("failed to transmit event {}", e),
        }
    }

    fn process_events(rx_channel: Receiver<ThreadEvent<T>>) {
        log::info!("processing events started");

        for rx_event in rx_channel.iter() {
            match rx_event {
                ThreadEvent::Exit => break,
                ThreadEvent::HandlerEvent(handler_event) => T::handle_event(handler_event),
            }
        }

        log::info!("processing events completed");
    }
}

impl<T> Drop for SageThread<T>
where
    T: ThreadHandler + 'static,
{
    fn drop(&mut self) {
        match self.stop() {
            Ok(_) => log::info!("stopped thread name: {}", self.thread_name),
            Err(e) => log::error!("failed to thread. {e}"),
        }
    }
}
