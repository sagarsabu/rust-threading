struct TcpServer;

impl sg_threading::Handler for TcpServer {
    type HandlerEvent = ();

    fn on_start(
        &mut self,
        thread: &mut sg_threading::Executor,
    ) -> Result<(), sg_errors::ErrorWrap> {
        thread.add_socket_listener("127.0.0.1:8080".parse().unwrap())?;

        Ok(())
    }

    fn on_handler_event(
        &mut self,
        _thread: &mut sg_threading::Executor,
        _event: Self::HandlerEvent,
    ) {
    }

    fn on_io_event(
        &mut self,
        _thread: &mut sg_threading::Executor,
        io_event: sg_threading::IoEvent,
    ) {
        log::info!(
            "peer: {} said: '{}'",
            io_event.socket_address,
            String::from_utf8_lossy(&io_event.data).trim()
        );
    }
}

fn main() -> Result<(), sg_errors::ErrorWrap> {
    sg_logging::setup_logger()?;

    let handler = sg_threading::Handle::new("tcp-server", || Box::new(TcpServer))?;
    handler.start();

    sg_threading::wait_for_exit(move || {
        handler.stop();
        Ok(())
    });

    Ok(())
}
