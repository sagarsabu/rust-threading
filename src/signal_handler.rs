use signal_hook::consts;
use std::sync::{self, atomic};

use crate::threading;

const SIGINT: usize = consts::SIGINT as usize;
const SIGTERM: usize = consts::SIGTERM as usize;
const SIGQUIT: usize = consts::SIGQUIT as usize;
const SIGNALS: &[usize] = &[SIGINT, SIGQUIT, SIGTERM];

#[derive(Clone)]
pub struct SignalHandler<T>
where
    T: threading::ThreadHandler,
{
    main: sync::Arc<threading::SageThread<T>>,
    signal: sync::Arc<atomic::AtomicUsize>,
}

impl<T> threading::ThreadHandler for SignalHandler<T>
where
    T: threading::ThreadHandler,
{
    type HandlerEvent = ();

    fn handle_event(&self, _: Self::HandlerEvent) {
        // noop
    }

    fn stopping(&self) {
        log::info!("stopping signal handler. invoking shutdown callback");
        self.main.stop();
    }

    fn process_events(
        &self,
        rx_channel: crossbeam_channel::Receiver<threading::ThreadEvent<Self>>,
    ) {
        log::info!("processing events started");

        let delta = std::time::Duration::from_millis(100);

        for sig in SIGNALS {
            signal_hook::flag::register_usize(*sig as i32, self.signal.clone(), *sig)
                .unwrap_or_else(|e| panic!("Failed to register signal: {}. err: {}", sig, e));
        }

        loop {
            match rx_channel.recv_timeout(delta) {
                Ok(rx_event) => match rx_event {
                    threading::ThreadEvent::Exit => {
                        log::info!("signal handler received exit event");
                        break;
                    }
                    // Its a noop
                    threading::ThreadEvent::HandlerEvent(_) => {}
                },
                Err(recv_err) => match recv_err {
                    crossbeam_channel::RecvTimeoutError::Disconnected => {
                        log::error!("rx_channel has disconnected");
                        break;
                    }
                    crossbeam_channel::RecvTimeoutError::Timeout => {
                        match self.signal.load(atomic::Ordering::Relaxed) {
                            0 => std::thread::sleep(delta),
                            sig @ (SIGINT | SIGTERM | SIGQUIT) => {
                                log::info!("caught signal: {}", sig);
                                break;
                            }
                            unexpected_signal => panic!("Unexpected signal: {}", unexpected_signal),
                        }
                    }
                },
            }
        }

        log::info!("processing events completed");
    }
}

impl<T> SignalHandler<T>
where
    T: threading::ThreadHandler,
{
    pub fn new(main: sync::Arc<threading::SageThread<T>>) -> Self {
        Self {
            main,
            signal: sync::Arc::<atomic::AtomicUsize>::new(0.into()),
        }
    }
}

pub fn wait_for_exit<T: threading::ThreadHandler>(signal_handler: &threading::SageThread<T>) {
    while signal_handler.is_running() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
