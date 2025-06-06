use sg_errors::ErrorWrap;
use std::{fmt::Display, io::Error};

pub type TimerID = usize;

#[tracing::instrument(level = "debug", skip(_u_context))]
fn action_handler(
    _signal: libc::c_int,
    _signal_info: *mut libc::siginfo_t,
    _u_context: *const libc::c_void,
) {
    tracing::debug!("timer expiry started");
    let inner = unsafe {
        let inner_raw = (*_signal_info).si_value().sival_ptr as *mut TimerInner;
        debug_assert!(!inner_raw.is_null());
        &mut (*inner_raw)
    };
    (inner.callback)();
    tracing::debug!("timer expiry completed");
}

#[derive(Debug)]
pub enum TimerType {
    Periodic,
    FireOnce,
}

pub struct Timer {
    name: String,
    pub(self) delta: std::time::Duration,
    pub(self) timer_type: TimerType,
    inner: *mut TimerInner,
}

struct TimerInner {
    timer_handle: libc::timer_t,
    callback: Box<dyn FnMut()>,
}

impl Timer {
    pub fn new(
        name: &str,
        delta: std::time::Duration,
        timer_type: TimerType,
        callback: Box<dyn FnMut()>,
    ) -> Result<Self, ErrorWrap> {
        let inner_boxed = Box::new(TimerInner {
            callback,
            timer_handle: std::ptr::null_mut(),
        });

        let inner = Box::into_raw(inner_boxed);

        let mut signal_event: libc::sigevent = unsafe { std::mem::zeroed() };
        signal_event.sigev_signo = libc::SIGRTMIN();
        signal_event.sigev_notify = libc::SIGEV_THREAD_ID;
        // invoked in same thread that created the timer
        signal_event.sigev_notify_thread_id = unsafe { libc::gettid() };
        signal_event.sigev_value.sival_ptr = inner as *mut libc::c_void;

        let mut signal_action: libc::sigaction = unsafe { std::mem::zeroed() };
        signal_action.sa_flags = libc::SA_SIGINFO;
        signal_action.sa_sigaction = action_handler as libc::sighandler_t;

        unsafe {
            libc::sigemptyset(&mut signal_action.sa_mask);
            if libc::sigaction(
                signal_event.sigev_signo,
                &signal_action,
                std::ptr::null_mut(),
            ) == -1
            {
                return Err(ErrorWrap::Io(Error::last_os_error()));
            }

            if libc::timer_create(
                libc::CLOCK_MONOTONIC,
                &mut signal_event,
                &mut (*inner).timer_handle,
            ) == -1
            {
                return Err(ErrorWrap::Io(Error::last_os_error()));
            }
        }

        let this_timer = Self {
            name: name.to_owned(),
            delta,
            timer_type,
            inner,
        };

        tracing::debug!("created timer:{}", this_timer);

        Ok(this_timer)
    }

    pub fn start(&self) -> Result<(), ErrorWrap> {
        let seconds = self.delta.as_secs() as i64;
        let ns = self.delta.subsec_nanos() as i64;
        unsafe {
            let mut timer_spec: libc::itimerspec = std::mem::zeroed();
            timer_spec.it_value.tv_sec = seconds;
            timer_spec.it_value.tv_nsec = ns;
            timer_spec.it_interval.tv_sec = match self.timer_type {
                TimerType::FireOnce => 0,
                TimerType::Periodic => seconds,
            };
            timer_spec.it_interval.tv_nsec = match self.timer_type {
                TimerType::FireOnce => 0,
                TimerType::Periodic => ns,
            };
            if libc::timer_settime(
                (*self.inner).timer_handle,
                0,
                &timer_spec,
                std::ptr::null_mut(),
            ) == -1
            {
                return Err(ErrorWrap::Io(Error::last_os_error()));
            }
        }

        tracing::debug!("started timer:{}", self);

        Ok(())
    }

    pub fn stop(&self) -> Result<(), ErrorWrap> {
        unsafe {
            let mut timer_spec: libc::itimerspec = std::mem::zeroed();
            timer_spec.it_interval.tv_sec = 0;
            timer_spec.it_interval.tv_nsec = 0;
            timer_spec.it_value.tv_sec = 0;
            timer_spec.it_value.tv_nsec = 0;
            if libc::timer_settime(
                (*self.inner).timer_handle,
                0,
                &timer_spec,
                std::ptr::null_mut(),
            ) == -1
            {
                return Err(ErrorWrap::Io(Error::last_os_error()));
            }
        }

        tracing::debug!("stopped timer:{}", self);

        Ok(())
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // reclaim ownership
        let inner = unsafe { Box::from_raw(self.inner) };

        let result = unsafe { libc::timer_delete(inner.timer_handle) };
        if result == -1 {
            tracing::error!(
                "failed to delete timer:{} error:{}",
                self,
                Error::last_os_error()
            );
        }

        drop(inner);

        tracing::debug!("dropped timer:{}", self);
    }
}

impl Display for Timer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "(name:'{}' type:'{}' period:{:?})",
            self.name,
            match self.timer_type {
                TimerType::FireOnce => "FireOnce",
                TimerType::Periodic => "Periodic",
            },
            self.delta,
        )
    }
}
