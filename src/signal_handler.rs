use signal_hook::consts;
use std::sync::{self, atomic};

use crate::errors::SageError;

pub fn register_exit_handler<F>(shutdown_callback: F) -> Result<(), SageError>
where
    F: Fn() + 'static + Send,
{
    std::thread::Builder::new()
        .name("ExitHandler".into())
        .spawn(move || {
            const SIGINT: usize = consts::SIGINT as usize;
            const SIGTERM: usize = consts::SIGTERM as usize;
            const SIGQUIT: usize = consts::SIGQUIT as usize;
            const EXIT_SIGNALS: &[usize] = &[SIGINT, SIGQUIT, SIGTERM];

            let signal = sync::Arc::<atomic::AtomicUsize>::new(0.into());
            let delta = std::time::Duration::from_millis(20);

            for sig in EXIT_SIGNALS {
                signal_hook::flag::register_usize(*sig as i32, signal.clone(), *sig)
                    .unwrap_or_else(|e| panic!("Failed to register signal: {}. err: {}", sig, e));
            }

            loop {
                match signal.load(atomic::Ordering::Relaxed) {
                    0 => std::thread::sleep(delta),
                    sig @ (SIGINT | SIGTERM | SIGQUIT) => {
                        log::info!("caught signal: {}", sig);
                        break;
                    }
                    unexpected_signal => panic!("Unexpected signal: {}", unexpected_signal),
                }
            }

            shutdown_callback();
        })
        .map_err(SageError::Io)?;

    Ok(())
}
