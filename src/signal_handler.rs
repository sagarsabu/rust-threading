use crate::timer::{Timer, TimerType};
use std::sync::{
    atomic::{self, Ordering},
    Mutex,
};

static SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static SIGNAL_CV: std::sync::Condvar = std::sync::Condvar::new();

fn signal_handler(signal: libc::c_int) {
    let sig_str = unsafe { std::ffi::CStr::from_ptr(libc::strsignal(signal)) };
    log::info!("signal handler received signal: {:?}", sig_str);
    SIGNAL.store(signal, Ordering::Relaxed);
    SIGNAL_CV.notify_one();
}

pub fn wait_for_exit<F>(shutdown_callback: F)
where
    F: FnOnce(),
{
    const EXIT_SIGNALS: &[libc::c_int] = &[libc::SIGINT, libc::SIGTERM, libc::SIGQUIT];

    for sig in EXIT_SIGNALS {
        unsafe {
            libc::signal(*sig, signal_handler as libc::sighandler_t);
        }
    }

    let mtx = Mutex::new(());
    let _guard = SIGNAL_CV
        .wait(mtx.lock().expect("Failed to lock mutex"))
        .expect("Failed to wait for signal condition variable");

    match SIGNAL.load(atomic::Ordering::Relaxed) {
        sig @ (libc::SIGINT | libc::SIGTERM | libc::SIGQUIT) => {
            let sig_str = unsafe { std::ffi::CStr::from_ptr(libc::strsignal(sig)) };
            log::info!("caught signal: {:?}", sig_str);
        }
        unexpected_signal => panic!("Unexpected signal: {}", unexpected_signal),
    }

    {
        // Start shutdown timer
        let shutdown_timer = Timer::new(
            "shutdown",
            std::time::Duration::from_millis(200),
            TimerType::FireOnce,
            || {
                log::error!("shutdown deadline exceeded. terminating...");
                std::process::exit(1);
            },
        )
        .unwrap_or_else(|e| {
            log::error!("failed to create shutdown timer. {}. exiting", e);
            std::process::exit(1);
        });

        shutdown_timer.start().unwrap_or_else(|e| {
            log::error!("failed to start shutdown timer. {}. exiting", e);
            std::process::exit(1);
        });

        shutdown_callback();

        shutdown_timer.stop().unwrap_or_else(|e| {
            log::error!("failed to stop shutdown timer. {}. exiting", e);
            std::process::exit(1);
        });
    }
}
