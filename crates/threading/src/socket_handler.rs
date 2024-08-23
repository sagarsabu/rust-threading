use std::{collections::HashMap, io::Read, net::SocketAddr};

use mio::{
    net::{TcpListener, TcpStream},
    Interest,
};
use sg_errors::ErrorWrap;

struct Connection {
    pub stream: TcpStream,
    pub read_buffer: Vec<u8>,
    pub write_buffer: Vec<u8>,
}

impl Connection {
    fn new(stream: TcpStream) -> Connection {
        Connection {
            stream,
            read_buffer: vec![],
            write_buffer: vec![],
        }
    }
}

pub struct IoEvent {
    pub socket_address: SocketAddr,
    pub data: Vec<u8>,
}

const LISTEN_TK: mio::Token = mio::Token(0);

pub struct TcpServerPoller {
    poller: mio::Poll,
    events: mio::Events,
    socket_address: SocketAddr,
    listener: TcpListener,
    active_streams: HashMap<mio::Token, Connection>,
    next_connection_token: usize,
}

impl TcpServerPoller {
    pub fn new(socket_address: SocketAddr) -> Result<Self, ErrorWrap> {
        let poller = mio::Poll::new()?;

        let mut handler = Self {
            poller,
            events: mio::Events::with_capacity(1_000_000),
            socket_address,
            listener: TcpListener::bind(socket_address)?,
            active_streams: HashMap::new(),
            next_connection_token: 1,
        };

        handler
            .poller
            .registry()
            .register(&mut handler.listener, LISTEN_TK, Interest::READABLE)?;

        log::info!("listening on tcp stream: {}", handler.socket_address);

        Ok(handler)
    }

    pub fn poll_events(&mut self) -> Result<Vec<IoEvent>, ErrorWrap> {
        let mut read_events = vec![];

        self.poller
            .poll(&mut self.events, Some(std::time::Duration::from_millis(20)))?;

        let mut removed_streams = vec![];

        for event in &self.events {
            match event.token() {
                LISTEN_TK => {
                    let (mut stream, address) = self.listener.accept()?;
                    log::info!("accepted new stream from {}", address);

                    let stream_tk = mio::Token(self.next_connection_token);
                    self.next_connection_token += 1;
                    self.poller
                        .registry()
                        .register(&mut stream, stream_tk, Interest::READABLE)?;
                    self.active_streams
                        .insert(stream_tk, Connection::new(stream));
                }
                maybe_stream_token => {
                    let mut remove_stream = false;

                    let connection = match self.active_streams.get_mut(&maybe_stream_token) {
                        None => {
                            log::error!("unknown stream token {:?}", maybe_stream_token);
                            continue;
                        }
                        Some(con) => con,
                    };

                    let peer_address = connection.stream.peer_addr()?;
                    if event.is_readable() {
                        use std::io::ErrorKind;
                        let mut buffer = [0; 1024];
                        match connection.stream.read(&mut buffer) {
                            // connection closed
                            Ok(0) => remove_stream = true,
                            Ok(n_bytes_read) => connection
                                .read_buffer
                                .extend_from_slice(&buffer[..n_bytes_read]),
                            Err(ref e) => match e.kind() {
                                // try again later
                                ErrorKind::WouldBlock => {}
                                // closed
                                ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset => {
                                    remove_stream = true
                                }
                                // unexpected error
                                _ => log::error!(
                                    "unexpected error reading from {} {}",
                                    peer_address,
                                    e
                                ),
                            },
                        }

                        if !remove_stream {
                            read_events.push(IoEvent {
                                socket_address: peer_address,
                                data: connection.read_buffer.clone(),
                            });
                        }

                        connection.read_buffer = vec![];
                    }

                    if remove_stream {
                        removed_streams.push(maybe_stream_token);
                    }
                }
            }
        }

        for removed_stream_tk in removed_streams {
            self.remove_connection(removed_stream_tk)?;
        }

        Ok(read_events)
    }

    fn remove_connection(&mut self, stream_token: mio::Token) -> Result<(), ErrorWrap> {
        if let Some(mut connection) = self.active_streams.remove(&stream_token) {
            log::info!(
                "removing connection to '{}'",
                connection.stream.peer_addr()?
            );
            connection.stream.shutdown(std::net::Shutdown::Both)?;
            self.poller.registry().deregister(&mut connection.stream)?;
        } else {
            log::error!("cannot remove connection to unknown {:?}", stream_token);
        }

        Ok(())
    }
}

impl Drop for TcpServerPoller {
    fn drop(&mut self) {
        let mut streams_to_remove = vec![];
        for k in self.active_streams.keys() {
            streams_to_remove.push(*k);
        }
        for s in streams_to_remove {
            if let Err(e) = self.remove_connection(s) {
                log::error!("failed to remove connection for {:?}. {}", s, e);
            }
        }
        if let Err(e) = self.poller.registry().deregister(&mut self.listener) {
            log::error!("failed to deregister listener. {}", e)
        }
    }
}
