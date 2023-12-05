use std::time;

pub struct ScopedDeadline {
    tag: String,
    start: time::Instant,
    deadline: time::Duration,
}

#[allow(dead_code)]
impl ScopedDeadline {
    pub fn new<T: AsRef<str>>(tag: T, deadline: time::Duration) -> Self {
        Self {
            tag: tag.as_ref().to_owned(),
            start: time::Instant::now(),
            deadline,
        }
    }
}

impl Drop for ScopedDeadline {
    fn drop(&mut self) {
        let dur = self.start.elapsed();
        let msg = format!("{} completed in {}ms", self.tag, dur.as_millis());
        if dur > self.deadline {
            log::warn!("{msg}");
        } else {
            log::debug!("{msg}");
        }
    }
}
