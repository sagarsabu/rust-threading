use signal_hook::consts;
use std::sync::{self, atomic, mpsc};

use crate::threading;

const SIGINT: usize = consts::SIGINT as usize;
const SIGTERM: usize = consts::SIGTERM as usize;
const SIGQUIT: usize = consts::SIGQUIT as usize;
const EXIT_SIGNALS: &[usize] = &[SIGINT, SIGQUIT, SIGTERM];

pub struct ExitHandler<F>
where
    F: Fn() + 'static + Sync + Send,
{
    signal: sync::Arc<atomic::AtomicUsize>,
    shutdown_callback: F,
}

impl<F> threading::ThreadHandler for ExitHandler<F>
where
    F: Fn() + 'static + Sync + Send,
{
    type HandlerEvent = ();

    fn handle_event(&self, _: Self::HandlerEvent) {
        // noop
    }

    fn stopping(&self) {
        log::info!("stopping signal handler. invoking shutdown callback");
        (self.shutdown_callback)();
    }

    fn process_events(&self, rx_channel: mpsc::Receiver<threading::ThreadEvent<Self>>) {
        log::info!("processing events started");

        let delta = std::time::Duration::from_millis(20);

        for sig in EXIT_SIGNALS {
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
                    mpsc::RecvTimeoutError::Disconnected => {
                        log::error!("rx_channel has disconnected");
                        break;
                    }
                    mpsc::RecvTimeoutError::Timeout => {
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

impl<F> ExitHandler<F>
where
    F: Fn() + 'static + Sync + Send,
{
    pub fn new(shutdown_callback: F) -> Self {
        Self {
            signal: sync::Arc::<atomic::AtomicUsize>::new(0.into()),
            shutdown_callback,
        }
    }
}
