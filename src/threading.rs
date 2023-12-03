use crate::errors::WorkerError;
use crossbeam_channel::{Receiver, Sender};
use std::thread;

pub trait Worker {
    type Event: Send + Sync;

    fn starting();

    fn stopping();

    fn handle_event(event: Self::Event) -> Result<(), WorkerError>;
}

pub struct SageThread<T>
where
    T: Worker + 'static,
{
    thread_name: String,
    tx_channel: Option<Sender<T::Event>>,
    rx_channel: Receiver<T::Event>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl<T> SageThread<T>
where
    T: Worker + 'static,
{
    pub fn new<StrLike: AsRef<str>>(thread_name: StrLike) -> Self {
        log::info!("creating thread {}", thread_name.as_ref());

        let (tx_channel, rx_channel) = crossbeam_channel::unbounded();

        Self {
            thread_name: thread_name.as_ref().to_owned(),
            tx_channel: Some(tx_channel),
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
            if let Some(_tx_channel) = self.tx_channel.take() {
                log::info!("dropping tx channel");
            }
            handle
                .join()
                .map_err(|e| WorkerError::Any(format!("{e:?}")))?;
        } else {
            log::error!("cannot stop worker that is not running");
        }

        Ok(())
    }

    pub fn transmit_event(&self, event: T::Event) -> Result<(), WorkerError> {
        if let Some(tx_channel) = &self.tx_channel {
            match tx_channel.send(event) {
                Ok(_) => log::debug!("transmitted event"),
                Err(e) => log::error!("failed to transmit event {}", e),
            }
        } else {
            log::error!("tx channel is none");
        }

        Ok(())
    }

    fn process_events(rx_channel: Receiver<T::Event>) {
        log::info!("processing events started");

        for rx_event in rx_channel.iter() {
            match T::handle_event(rx_event) {
                Ok(_) => log::debug!("handled event"),
                Err(e) => log::error!("failed to handle event. {}", e),
            }
        }

        log::info!("processing events completed");
    }
}

impl<T> Drop for SageThread<T>
where
    T: Worker + 'static,
{
    fn drop(&mut self) {
        match self.stop() {
            Ok(_) => log::info!("stopped worker name: {}", self.thread_name),
            Err(e) => log::error!("failed to worker. {e}"),
        }
    }
}
