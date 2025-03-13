use sg_errors::ErrorWrap;
use std::{
    fmt::Display,
    io::Error,
    sync::atomic::{AtomicUsize, Ordering},
};

pub type TimerID = usize;

fn action_handler(
    _signal: libc::c_int,
    _signal_info: *mut libc::siginfo_t,
    _u_context: *const libc::c_void,
) {
    log::debug!("timer expiry started");
    let inner = unsafe {
        let inner_raw = (*_signal_info).si_value().sival_ptr as *mut TimerInner;
        &mut (*inner_raw)
    };
    (inner.callback)();
    log::debug!("timer expiry completed");
}

fn get_next_timer_id() -> TimerID {
    static TIMER_ID: AtomicUsize = AtomicUsize::new(0);
    // automatically wraps around
    TIMER_ID.fetch_add(1, Ordering::Relaxed)
}

pub enum TimerType {
    Periodic,
    FireOnce,
}

pub struct Timer {
    id: usize,
    name: String,
    pub(self) delta: std::time::Duration,
    pub(self) timer_type: TimerType,
    inner: *mut TimerInner,
}

struct TimerInner {
    timer_id: libc::timer_t,
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
            timer_id: std::ptr::null_mut(),
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
                &mut (*inner).timer_id,
            ) == -1
            {
                return Err(ErrorWrap::Io(Error::last_os_error()));
            }
        }

        let this_timer = Self {
            id: get_next_timer_id(),
            name: name.to_owned(),
            delta,
            timer_type,
            inner,
        };

        log::debug!("created timer:{}", this_timer);

        Ok(this_timer)
    }

    pub fn get_id(&self) -> TimerID {
        self.id
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
            if libc::timer_settime((*self.inner).timer_id, 0, &timer_spec, std::ptr::null_mut())
                == -1
            {
                return Err(ErrorWrap::Io(Error::last_os_error()));
            }
        }

        log::debug!("started timer:{}", self);

        Ok(())
    }

    pub fn stop(&self) -> Result<(), ErrorWrap> {
        unsafe {
            let mut timer_spec: libc::itimerspec = std::mem::zeroed();
            timer_spec.it_interval.tv_sec = 0;
            timer_spec.it_interval.tv_nsec = 0;
            timer_spec.it_value.tv_sec = 0;
            timer_spec.it_value.tv_nsec = 0;
            if libc::timer_settime((*self.inner).timer_id, 0, &timer_spec, std::ptr::null_mut())
                == -1
            {
                return Err(ErrorWrap::Io(Error::last_os_error()));
            }
        }

        log::debug!("stopped timer:{}", self);

        Ok(())
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // reclaim ownership
        let inner = unsafe { Box::from_raw(self.inner) };

        let result = unsafe { libc::timer_delete(inner.timer_id) };
        if result == -1 {
            log::error!(
                "failed to delete timer:{} error:{}",
                self,
                Error::last_os_error()
            );
        }

        log::debug!("dropped timer:{}", self);
    }
}

impl Display for Timer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "(id:{} name:'{}' type:'{}' period:{:?})",
            self.id,
            self.name,
            match self.timer_type {
                TimerType::FireOnce => "FireOnce",
                TimerType::Periodic => "Periodic",
            },
            self.delta,
        )
    }
}
