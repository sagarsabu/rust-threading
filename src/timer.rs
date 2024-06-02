use crate::errors::SageError;
use std::{
    fmt::Display,
    io::Error,
    sync::{atomic::Ordering, Arc, Condvar, Mutex, Once, OnceLock},
    thread::JoinHandle,
};

type TimerCV = Arc<(Mutex<bool>, Condvar)>;
pub type TimerId = usize;
pub type TransmitCallbackType = Arc<dyn Fn() + 'static + Send + Sync>;
pub type TimerCallbackType = Arc<dyn Fn() + 'static + Send + Sync>;
pub type TimerCollection = std::collections::HashMap<TimerId, Arc<Mutex<Timer>>>;

static TIMER_TID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

// The signal handler. There's some real weird shit happening in here regarding memory access
fn action_handler(
    _signal: libc::c_int,
    _signal_info: *mut libc::siginfo_t,
    _user_data: *const libc::c_void,
) {
    log::debug!("notifying timer thread");
    let (_lock, cv) = &(**get_timer_cv());
    cv.notify_one();
    log::debug!("notified timer thread");
}

fn get_next_timer_id() -> TimerId {
    static mut TIMER_ID: OnceLock<TimerId> = OnceLock::new();
    let timer_id = unsafe {
        TIMER_ID.get_or_init(|| 0);
        TIMER_ID.get_mut().unwrap()
    };

    if (*timer_id) >= TimerId::MAX - 1 {
        (*timer_id) = 0;
    }

    (*timer_id) += 1;
    *timer_id
}

fn get_timer_cv() -> &'static TimerCV {
    static TIMER_CV: OnceLock<TimerCV> = OnceLock::new();
    TIMER_CV.get_or_init(|| TimerCV::new((Mutex::new(false), Condvar::new())))
}

fn get_all_timers() -> &'static mut TimerCollection {
    static mut TIMERS: OnceLock<TimerCollection> = OnceLock::new();
    unsafe {
        TIMERS.get_or_init(|| TimerCollection::new());
        TIMERS.get_mut().unwrap()
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
    pub(self) last_expiry: std::time::Instant,
    pub(self) delta: std::time::Duration,
    pub(self) timer_type: TimerType,
    pub(self) transmit_callback: TransmitCallbackType,
    pub(self) running: bool,
    inner: TimerInner,
}

struct TimerInner {
    timer_id: libc::timer_t,
}

unsafe impl Send for TimerInner {}
unsafe impl Sync for TimerInner {}

impl Timer {
    pub fn new<TxFunc>(
        name: &str,
        delta: std::time::Duration,
        timer_type: TimerType,
        transmit_callback: TxFunc,
    ) -> Result<Arc<Mutex<Self>>, SageError>
    where
        TxFunc: Fn() + 'static + Send + Sync,
    {
        spin_up_timer_thread();

        let mut this_timer = Self {
            id: get_next_timer_id(),
            name: name.to_owned(),
            last_expiry: std::time::Instant::now(),
            delta,
            timer_type,
            transmit_callback: Arc::from(transmit_callback),
            running: false,
            inner: TimerInner {
                timer_id: unsafe { std::mem::zeroed() },
            },
        };
        log::info!("creating timer:{}", this_timer);

        let mut signal_event: libc::sigevent = unsafe { std::mem::zeroed() };
        signal_event.sigev_signo = libc::SIGRTMIN();
        signal_event.sigev_notify = libc::SIGEV_THREAD_ID;
        signal_event.sigev_notify_thread_id = TIMER_TID.load(Ordering::SeqCst);
        signal_event.sigev_value.sival_ptr = std::ptr::null_mut();

        let mut signal_action: libc::sigaction = unsafe { std::mem::zeroed() };
        signal_action.sa_flags = libc::SA_SIGINFO;
        signal_action.sa_sigaction = action_handler as usize;

        unsafe {
            libc::sigemptyset(&mut signal_action.sa_mask);
            if libc::sigaction(
                signal_event.sigev_signo,
                &signal_action,
                std::ptr::null_mut(),
            ) == -1
            {
                return Err(SageError::Io(Error::last_os_error()));
            }

            if libc::timer_create(
                libc::CLOCK_MONOTONIC,
                &mut signal_event,
                &mut this_timer.inner.timer_id,
            ) == -1
            {
                return Err(SageError::Io(Error::last_os_error()));
            }
        }

        log::info!("created timer:{}", this_timer);
        Ok(Arc::new(Mutex::new(this_timer)))
    }

    pub fn get_id(&self) -> TimerId {
        self.id
    }

    pub fn start(timer: &Arc<Mutex<Self>>) -> Result<(), SageError> {
        let this_timer = &mut timer.lock().map_err(SageError::to_generic)?;

        let seconds = this_timer.delta.as_secs() as i64;
        let ns = this_timer.delta.subsec_nanos() as i64;
        unsafe {
            let mut timer_spec: libc::itimerspec = std::mem::zeroed();
            timer_spec.it_value.tv_sec = seconds;
            timer_spec.it_value.tv_nsec = ns;
            timer_spec.it_interval.tv_sec = match this_timer.timer_type {
                TimerType::FireOnce => 0,
                TimerType::Periodic => seconds,
            };
            timer_spec.it_interval.tv_nsec = match this_timer.timer_type {
                TimerType::FireOnce => 0,
                TimerType::Periodic => ns,
            };
            if libc::timer_settime(
                this_timer.inner.timer_id,
                0,
                &timer_spec,
                std::ptr::null_mut(),
            ) == -1
            {
                return Err(SageError::Io(Error::last_os_error()));
            }
        }

        this_timer.running = true;
        get_all_timers().insert(this_timer.id, timer.clone());
        log::info!("started timer:{}", this_timer);
        Ok(())
    }

    pub fn stop(timer: &Arc<Mutex<Self>>) -> Result<(), SageError> {
        let this_timer = &mut timer.lock().map_err(SageError::to_generic)?;

        unsafe {
            let mut timer_spec: libc::itimerspec = std::mem::zeroed();
            timer_spec.it_interval.tv_sec = 0;
            timer_spec.it_interval.tv_nsec = 0;
            timer_spec.it_value.tv_sec = 0;
            timer_spec.it_value.tv_nsec = 0;
            if libc::timer_settime(
                this_timer.inner.timer_id,
                0,
                &timer_spec,
                std::ptr::null_mut(),
            ) == -1
            {
                return Err(SageError::Io(Error::last_os_error()));
            }
        }

        this_timer.running = false;
        get_all_timers().remove(&this_timer.id);
        log::info!("stopped timer:{}", this_timer);
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

        log::info!("dropped timer:{}", self);
    }
}

impl Display for Timer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "(id:{} name:'{}' type:'{}' period:{}us since-last-expiry:{}us)",
            self.id,
            self.name,
            match self.timer_type {
                TimerType::FireOnce => "FireOnce",
                TimerType::Periodic => "Periodic",
            },
            self.delta.as_micros(),
            self.last_expiry.elapsed().as_micros()
        )
    }
}

fn timer_thread_entry(start_barrier: Arc<std::sync::Barrier>) {
    const DELTA_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_micros(250);
    TIMER_TID.store(unsafe { libc::gettid() }, Ordering::SeqCst);
    start_barrier.wait();

    log::info!("timer thread created");
    let (lock, cv) = &(**get_timer_cv());
    loop {
        let _guard = cv.wait(lock.lock().unwrap()).unwrap();
        let now = std::time::Instant::now();
        log::info!("timer thread notified");

        for (_id, timer) in get_all_timers() {
            match timer.lock() {
                Ok(mut timer) => {
                    let time_elapsed = now - timer.last_expiry;
                    let expired = (time_elapsed >= timer.delta - DELTA_GRACE_PERIOD)
                        || ((time_elapsed >= timer.delta - DELTA_GRACE_PERIOD)
                            && (time_elapsed <= timer.delta + DELTA_GRACE_PERIOD));
                    log::info!("timer thread invoking timer:{} expired:{}", timer, expired);
                    assert!(expired == true);

                    if timer.running && expired {
                        log::info!("timer thread invoking timer:{}", timer);
                        (timer.transmit_callback)();
                        log::info!("timer thread invoked timer:{}", timer);
                        timer.last_expiry = now;

                        if timer.timer_type == TimerType::FireOnce {
                            timer.running = false;
                        }
                    }
                }

                Err(e) => log::error!("failed to lock timer data. {}", e),
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
            .name("Timer".into())
            .spawn(move || timer_thread_entry(start_barrier_cp))
            .unwrap();

        start_barrier.wait();
    });
}
