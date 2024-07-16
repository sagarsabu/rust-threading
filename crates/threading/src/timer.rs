use crate::scoped_deadline::ScopedDeadline;
use sg_errors::ErrorWrap;
use std::{
    fmt::Display,
    io::Error,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, Once, OnceLock,
    },
    thread::JoinHandle,
};

type TimerCV = Arc<(Mutex<bool>, Condvar)>;
pub type TimerId = usize;
pub type TimerCallback = Arc<dyn Fn() + 'static + Send + Sync>;
pub type TimerCollection = std::collections::HashMap<TimerId, Arc<Timer>>;

static TIMER_TID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn action_handler(
    _signal: libc::c_int,
    _signal_info: *mut libc::siginfo_t,
    _u_context: *const libc::c_void,
) {
    log::debug!("notifying timer thread");
    let (_lock, cv) = &(**get_timer_condition_var());
    let expired = unsafe {
        let expired_ptr = (*_signal_info).si_value().sival_ptr as *mut AtomicBool;
        &(*expired_ptr)
    };
    expired.store(true, Ordering::Release);
    cv.notify_one();
    log::debug!("notified timer thread");
}

fn get_next_timer_id() -> TimerId {
    static mut TIMER_ID: OnceLock<TimerId> = OnceLock::new();
    let timer_id = unsafe {
        TIMER_ID.get_or_init(|| 0);
        TIMER_ID
            .get_mut()
            .expect("Failed to get mut ref to timer id")
    };

    if (*timer_id) >= TimerId::MAX - 1 {
        (*timer_id) = 0;
    }

    (*timer_id) += 1;
    *timer_id
}

fn get_timer_condition_var() -> &'static TimerCV {
    static TIMER_CV: OnceLock<TimerCV> = OnceLock::new();
    TIMER_CV.get_or_init(|| TimerCV::new((Mutex::new(false), Condvar::new())))
}

fn get_all_timers() -> &'static mut TimerCollection {
    static mut TIMERS: OnceLock<TimerCollection> = OnceLock::new();
    unsafe {
        TIMERS.get_or_init(TimerCollection::new);
        TIMERS
            .get_mut()
            .expect("Failed to get mut ref to timer collection")
    }
}

#[derive(PartialEq)]
pub enum TimerType {
    Periodic,
    FireOnce,
}

pub struct Timer {
    id: usize,
    name: String,
    pub(self) delta: std::time::Duration,
    pub(self) timer_type: TimerType,
    pub(self) callback: TimerCallback,
    pub(self) expired: Arc<AtomicBool>,
    inner: TimerInner,
}

struct TimerInner {
    timer_id: libc::timer_t,
}

// FFI is inherently unsafe
unsafe impl Send for TimerInner {}
unsafe impl Sync for TimerInner {}

impl Timer {
    pub fn new<Func>(
        name: &str,
        delta: std::time::Duration,
        timer_type: TimerType,
        timer_callback: Func,
    ) -> Result<Arc<Self>, ErrorWrap>
    where
        Func: Fn() + 'static + Send + Sync,
    {
        spin_up_timer_thread();

        let mut this_timer = Self {
            id: get_next_timer_id(),
            name: name.to_owned(),
            delta,
            timer_type,
            expired: Arc::new(AtomicBool::new(false)),
            callback: Arc::from(timer_callback),
            inner: TimerInner {
                timer_id: unsafe { std::mem::zeroed() },
            },
        };

        log::debug!("creating timer:{}", this_timer);

        let mut signal_event: libc::sigevent = unsafe { std::mem::zeroed() };
        signal_event.sigev_signo = libc::SIGRTMIN();
        signal_event.sigev_notify = libc::SIGEV_THREAD_ID;
        signal_event.sigev_notify_thread_id = TIMER_TID.load(Ordering::Relaxed);
        signal_event.sigev_value.sival_ptr = this_timer.expired.as_ptr() as *mut libc::c_void;

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
                &mut this_timer.inner.timer_id,
            ) == -1
            {
                return Err(ErrorWrap::Io(Error::last_os_error()));
            }
        }

        log::debug!("created timer:{}", this_timer);
        Ok(Arc::new(this_timer))
    }

    pub fn get_id(&self) -> TimerId {
        self.id
    }

    pub fn start(self: &Arc<Self>) -> Result<(), ErrorWrap> {
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
            if libc::timer_settime(self.inner.timer_id, 0, &timer_spec, std::ptr::null_mut()) == -1
            {
                return Err(ErrorWrap::Io(Error::last_os_error()));
            }
        }

        get_all_timers().insert(self.id, self.clone());
        log::debug!("started timer:{}", self);
        Ok(())
    }

    pub fn stop(self: &Arc<Self>) -> Result<(), ErrorWrap> {
        unsafe {
            let mut timer_spec: libc::itimerspec = std::mem::zeroed();
            timer_spec.it_interval.tv_sec = 0;
            timer_spec.it_interval.tv_nsec = 0;
            timer_spec.it_value.tv_sec = 0;
            timer_spec.it_value.tv_nsec = 0;
            if libc::timer_settime(self.inner.timer_id, 0, &timer_spec, std::ptr::null_mut()) == -1
            {
                return Err(ErrorWrap::Io(Error::last_os_error()));
            }
        }

        get_all_timers().remove(&self.id);
        log::debug!("stopped timer:{}", self);
        Ok(())
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let result = unsafe { libc::timer_delete(self.inner.timer_id) };
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

fn timer_thread_entry(start_barrier: Arc<std::sync::Barrier>) {
    TIMER_TID.store(unsafe { libc::gettid() }, Ordering::SeqCst);
    start_barrier.wait();

    log::info!("timer thread created");
    let (lock, cv) = &(**get_timer_condition_var());
    loop {
        let _guard = cv
            .wait(lock.lock().expect("Failed to lock timer thread CV"))
            .expect("Failed to wait for timer thread CV");

        log::debug!("timer thread notified");

        for timer in get_all_timers().values_mut() {
            let expired = timer.expired.as_ref();
            if !expired.load(Ordering::Acquire) {
                continue;
            }
            expired.store(false, Ordering::Release);

            {
                let _dl = ScopedDeadline::new(
                    format!("timer-cb-dl-{}", timer.name),
                    std::time::Duration::from_millis(100),
                );
                (timer.callback)();
            }
        }
    }
}

fn spin_up_timer_thread() {
    static TIMER_THREAD_STARTED: Once = Once::new();
    TIMER_THREAD_STARTED.call_once(|| {
        let start_barrier = Arc::new(std::sync::Barrier::new(2));
        let start_barrier_cp = start_barrier.clone();
        let _handle: JoinHandle<()> = std::thread::Builder::new()
            .name("timer-thread".into())
            .spawn(move || timer_thread_entry(start_barrier_cp))
            .expect("Failed to start timer thread");

        start_barrier.wait();
    });
}
