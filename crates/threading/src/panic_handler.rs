use std::sync::Once;

// Ensure's all threads exit on panic
pub fn register_panic_handler() {
    static REGISTERED_PANIC_HANDLER: Once = Once::new();
    REGISTERED_PANIC_HANDLER.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            default_hook(panic_info);
            std::process::exit(1);
        }));
    })
}
