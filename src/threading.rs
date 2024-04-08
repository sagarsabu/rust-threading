use std::{
    sync::{self, atomic, mpsc, Arc},
    thread,
};

use crate::errors::SageError;

pub trait ThreadHandler: 'static + Send {
    type HandlerEvent: Send;

    fn starting(&self) {
        log::info!("default starting handler");
    }

    fn stopping(&self) {
        log::info!("default stopping handler");
    }

    fn handle_event(&self, event: Self::HandlerEvent);
}

enum ThreadEvent<T: ThreadHandler> {
    HandlerEvent(T::HandlerEvent),
    Exit,
}

pub struct SageThread<T: ThreadHandler> {
    pub name: String,
    tx_channel: mpsc::Sender<ThreadEvent<T>>,
    start_barrier: Arc<sync::Barrier>,
    running: Arc<atomic::AtomicBool>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl<T: ThreadHandler> SageThread<T> {
    pub fn new(name: &str, handler: T) -> Result<Self, SageError> {
        log::info!("[{}] creating handler", name);

        let (tx_channel, rx_channel) = mpsc::channel();

        let running = Arc::new(atomic::AtomicBool::new(false));
        let running_cp = running.clone();

        let start_barrier = Arc::new(sync::Barrier::new(2));
        let start_barrier_cp = start_barrier.clone();

        let handle = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                start_barrier_cp.wait();

                running_cp.store(true, atomic::Ordering::Relaxed);

                handler.starting();
                Self::process_events(&handler, rx_channel);
                handler.stopping();

                running_cp.store(false, atomic::Ordering::Relaxed);
            })?;

        Ok(Self {
            name: name.to_string(),
            tx_channel,
            thread_handle: Some(handle),
            running,
            // Waiting from inside the thread and the start entry
            start_barrier,
        })
    }

    pub fn start(&self) {
        log::info!("[{}] dispatching start for handler", self.name);
        // Fill up the barrier
        self.start_barrier.wait();
    }

    fn process_events(handler: &T, rx_channel: mpsc::Receiver<ThreadEvent<T>>) {
        log::info!("processing events started");

        for rx_event in rx_channel.iter() {
            match rx_event {
                ThreadEvent::Exit => break,
                ThreadEvent::HandlerEvent(handler_event) => handler.handle_event(handler_event),
            }
        }

        log::info!("processing events completed");
    }

    pub fn stop(&self) {
        log::info!("[{}] terminating handler", self.name);
        self.terminate();
    }

    pub fn is_running(&self) -> bool {
        self.running.load(atomic::Ordering::Relaxed)
    }

    pub fn transmit_event(&self, event: T::HandlerEvent) {
        match self.tx_channel.send(ThreadEvent::HandlerEvent(event)) {
            Ok(_) => log::debug!("[{}] transmitted event to handler", self.name),
            Err(e) => log::error!("[{}] failed to transmit event {} to handler", self.name, e),
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
                    "[{}] failed to send exit event to handler. attempt: {}/{}. trying again.",
                    self.name,
                    exit_attempt,
                    MAX_EXIT_ATTEMPTS
                );
            }
        } else {
            log::warn!("[{}] cannot stop handler that is not running", self.name);
        }
    }
}

impl<T: ThreadHandler> Drop for SageThread<T> {
    fn drop(&mut self) {
        log::info!("[{}] dropping handler", self.name);

        if self.is_running() {
            self.stop();
        }

        if let Some(thread_handle) = self.thread_handle.take() {
            match thread_handle.join() {
                Ok(_) => log::info!("[{}] joined thread for handler", self.name),
                Err(e) => log::error!("[{}] failed to join thread for handler. {:?}", self.name, e),
            };
        } else {
            log::error!("[{}] join handle does not exist for handler", self.name);
        }
    }
}
