use tracing_subscriber::fmt::format::FmtSpan;

pub fn setup_logger() {
    tracing_subscriber::fmt()
        .with_level(true)
        .with_ansi(true)
        .with_thread_names(true)
        .with_target(false)
        .with_span_events(FmtSpan::CLOSE)
        .compact()
        .init();
}
