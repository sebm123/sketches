use std::io::prelude::*;
use std::io::{BufRead, BufReader, Error, ErrorKind, Result};
use std::net::SocketAddr;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use mio::net::TcpStream;
use mio::{Events, Poll, PollOpt, Ready, Token};

use crate::proto::RawMessage;

pub trait IrcWriter {
    fn write_message(&mut self, msg: &RawMessage) -> Result<()>;
}

pub trait IrcReader {
    fn read_message(&mut self) -> Result<RawMessage>;
}

impl<T: Write> IrcWriter for T {
    fn write_message(&mut self, msg: &RawMessage) -> Result<()> {
        let msg_str = msg.to_string();
        self.write_all(msg_str.as_bytes())?;
        self.write_all(b"\r\n")?;

        Ok(())
    }
}

impl<T: BufRead> IrcReader for T {
    fn read_message(&mut self) -> Result<RawMessage> {
        let mut buf = String::new();
        self.read_line(&mut buf)?;

        let line = buf.trim_end_matches(|c| c == '\r' || c == '\n');
        RawMessage::parse(line).ok_or_else(|| {
            Error::new(ErrorKind::Other, "failed to parse message from line")
        })
    }
}

#[derive(Clone)]
pub struct IrcChannel(pub Sender<RawMessage>);

impl IrcChannel {
    pub fn new() -> (Self, Receiver<RawMessage>) {
        let (sender, receiver) = unbounded();
        (IrcChannel(sender), receiver)
    }
}

impl IrcWriter for IrcChannel {
    fn write_message(&mut self, msg: &RawMessage) -> Result<()> {
        if let Err(err) = self.0.send(msg.clone()) {
            println!("Failed to write to client: {:?}", err);
            return Err(Error::new(ErrorKind::Other, "other end disconnected"));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct IrcSocketConfig {
    // TODO: stop being lazy, make this &str
    pub addr: String,
    pub max_retries: Option<usize>,
}

pub enum SocketEvent {
    Connected,
    Disconnected,
    Received(RawMessage),
}

// TODO: This becomes IrcSocket
pub struct DumbIrcSocket {
    to_socket: (Sender<RawMessage>, Receiver<RawMessage>),
    from_socket: Sender<SocketEvent>,
}

impl DumbIrcSocket {
    pub fn new(from_socket: Sender<SocketEvent>) -> Self {
        Self {
            from_socket,
            to_socket: unbounded(),
        }
    }

    pub fn socket_sender(&self) -> Sender<RawMessage> {
        self.to_socket.0.clone()
    }

    pub fn start(
        &mut self,
        read_socket: impl Read + Send + 'static,
        write_socket: &mut (impl Write + Send),
    ) -> Result<()> {
        let recv_err = || Error::new(ErrorKind::Other, "receiver disconnected");

        let from_socket = self.from_socket.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(read_socket);

            // TODO: Atomic boolean for shutdown.
            loop {
                let result = reader.read_message().and_then(|msg| {
                    println!("[birch <- \u{1b}[37;1mnet\u{1b}[0m] {}", msg);
                    from_socket
                        .send(SocketEvent::Received(msg))
                        .map_err(|_| recv_err())
                });

                if let Err(err) = result {
                    println!("Read from socket failed: {:?}", err);
                    break;
                }
            }
        });

        self.from_socket
            .send(SocketEvent::Connected)
            .map_err(|_| recv_err())?;

        for msg in self.to_socket.1.clone() {
            println!("[\u{1b}[37;1mbirch\u{1b}[0m -> net] {}", msg);
            write_socket.write_message(&msg)?;
        }

        self.from_socket
            .send(SocketEvent::Disconnected)
            .map_err(|_| recv_err())?;

        Ok(())
    }
}

// TODO: ReconnectingIrcSocket, PersistentIrcSocket, something to that
// effect.
pub struct IrcSocket {
    config: IrcSocketConfig,

    to_network: (Sender<RawMessage>, Receiver<RawMessage>),
    from_network: Sender<SocketEvent>,
}

impl IrcSocket {
    pub fn new(config: IrcSocketConfig, from_network: Sender<SocketEvent>) -> Self {
        let to_network = unbounded();

        Self {
            config,
            to_network,
            from_network,
        }
    }

    pub fn network_channel(&self) -> Sender<RawMessage> {
        self.to_network.0.clone()
    }

    /// Repeatedly (up to the configured maximum retries) try to
    /// establish a connection to the specied address.
    ///
    /// Eventually this is will include a sleep / exponential backoff
    /// (TODO: that)
    fn try_create_stream(&self) -> Result<TcpStream> {
        let max_retries = self.config.max_retries;

        let mut i = 0;
        loop {
            let stream = std::net::TcpStream::connect(&self.config.addr);
            let over_max_tries = max_retries.map(|max| i > max).unwrap_or(false);

            match stream {
                Ok(stream) => return TcpStream::from_stream(stream),
                Err(err) => {
                    if over_max_tries {
                        return Err(err);
                    }
                }
            }

            i += 1
        }
    }

    /// On connection close, return `true` when the connection should
    /// be restarted, and false otherwise. If there is a
    /// non-recoverable exception, return the error.
    ///
    /// TODO: Need to come up with the clean exit concept.
    fn connect(&mut self) -> Result<bool> {
        // TODO: reimplement this
        Ok(true)
    }

    pub fn start_loop(&mut self) -> Result<()> {
        loop {
            match self.connect() {
                Ok(true) => println!("connection terminated, restarting"),
                Ok(false) => break,
                Err(err) => {
                    println!("connection failed: {:?}", err);
                    return Err(err);
                }
            }
        }
        Ok(())
    }
}
