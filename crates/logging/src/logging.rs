use sg_errors::ErrorWrap;

mod format {
    #![allow(dead_code)]

    // Formatter control

    pub(super) const FORMAT_END: &str = "\x1B[00m";
    pub(super) const FORMAT_BOLD: &str = "\x1B[01m";
    pub(super) const FORMAT_DISABLED: &str = "\x1B[02m";
    pub(super) const FORMAT_ITALIC: &str = "\x1B[03m";
    pub(super) const FORMAT_URL: &str = "\x1B[04m";
    pub(super) const FORMAT_BLINK: &str = "\x1B[05m";
    pub(super) const FORMAT_BLINK2: &str = "\x1B[06m";
    pub(super) const FORMAT_SELECTED: &str = "\x1B[07m";
    pub(super) const FORMAT_INVISIBLE: &str = "\x1B[08m";
    pub(super) const FORMAT_STRIKE: &str = "\x1B[09m";
    pub(super) const FORMAT_DOUBLE_UNDERLINE: &str = "\x1B[21m";

    // Dark Colours

    pub(super) const DARK_BLACK: &str = "\x1B[30m";
    pub(super) const DARK_RED: &str = "\x1B[31m";
    pub(super) const DARK_GREEN: &str = "\x1B[32m";
    pub(super) const DARK_YELLOW: &str = "\x1B[33m";
    pub(super) const DARK_BLUE: &str = "\x1B[34m";
    pub(super) const DARK_VIOLET: &str = "\x1B[35m";
    pub(super) const DARK_BEIGE: &str = "\x1B[36m";
    pub(super) const DARK_WHITE: &str = "\x1B[37m";

    // Light Colours

    pub(super) const LIGHT_GREY: &str = "\x1B[90m";
    pub(super) const LIGHT_RED: &str = "\x1B[91m";
    pub(super) const LIGHT_GREEN: &str = "\x1B[92m";
    pub(super) const LIGHT_YELLOW: &str = "\x1B[93m";
    pub(super) const LIGHT_BLUE: &str = "\x1B[94m";
    pub(super) const LIGHT_VIOLET: &str = "\x1B[95m";
    pub(super) const LIGHT_BEIGE: &str = "\x1B[96m";
    pub(super) const LIGHT_WHITE: &str = "\x1B[97m";
}

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

fn get_this_thread_name() -> String {
    let current_thread = std::thread::current();
    let thread_name = current_thread.name().unwrap_or("-");
    thread_name.into()
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        thread_local!(
            static THREAD_NAME: std::cell::RefCell<String> =
                std::cell::RefCell::new(get_this_thread_name());
        );

        let format_start = match record.level() {
            log::Level::Trace => format::LIGHT_GREEN,
            log::Level::Debug => format::DARK_BLUE,
            log::Level::Info => format::DARK_WHITE,
            log::Level::Warn => format::LIGHT_YELLOW,
            log::Level::Error => format::DARK_RED,
        };

        THREAD_NAME.with_borrow(|thread_name: &String| {
            println!(
                "{}{} [{:^7}] [{:^15}] {}{}",
                format_start,
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S:%3f"),
                record.level(),
                thread_name,
                record.args(),
                format::FORMAT_END
            )
        });
    }

    fn flush(&self) {
        // Noop
    }
}

pub fn setup_logger() -> Result<(), ErrorWrap> {
    let logger = Box::new(Logger::new(log::Level::Info));
    log::set_max_level(logger.get_level_filter());
    log::set_boxed_logger(logger).map_err(ErrorWrap::to_generic)?;

    Ok(())
}
