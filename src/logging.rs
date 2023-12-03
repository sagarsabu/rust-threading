pub struct Logger {
    level: log::Level,
    level_filer: log::LevelFilter,
}

impl Logger {
    pub fn new(level: log::Level) -> Self {
        Self {
            level,
            level_filer: level.to_level_filter(),
        }
    }

    pub fn get_level_filter(&self) -> log::LevelFilter {
        self.level_filer
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let binding = std::thread::current();
        let thread_name = binding.name().unwrap_or("-");

        println!(
            "{} [{:<5}] [{}] {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S:%3f"),
            record.level(),
            thread_name,
            record.args()
        );
    }

    fn flush(&self) {
        // Noop
    }
}
