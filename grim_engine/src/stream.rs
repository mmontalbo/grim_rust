use std::io::{self, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use grim_stream::{encode_message, Hello, MessageKind, StateUpdate};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StreamError {
    #[error("stream worker disconnected")]
    Disconnected,
    #[error("message encode failed: {0}")]
    Encode(#[from] grim_stream::ProtocolError),
}

enum Command {
    Send(Vec<u8>),
    Shutdown,
}

/// Broadcasts GrimStream messages to a single connected subscriber.
pub struct StreamServer {
    sender: Sender<Command>,
    start: Instant,
    seq: AtomicU64,
}

impl StreamServer {
    pub fn bind<A: ToSocketAddrs>(addr: A, build: Option<String>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(addr).context("binding stream socket")?;
        listener
            .set_nonblocking(true)
            .context("setting stream listener non-blocking")?;
        let (tx, rx) = mpsc::channel();
        let build_info = build.unwrap_or_else(|| "dev".to_string());
        thread::Builder::new()
            .name("grim_stream".to_string())
            .spawn(move || worker_loop(listener, rx, build_info))
            .context("spawning stream worker thread")?;
        Ok(Self {
            sender: tx,
            start: Instant::now(),
            seq: AtomicU64::new(0),
        })
    }

    pub fn send_state_update(&self, mut update: StateUpdate) -> Result<(), StreamError> {
        update.seq = self.seq.fetch_add(1, Ordering::Relaxed);
        if update.host_time_ns == 0 {
            update.host_time_ns = self.start.elapsed().as_nanos() as u64;
        }
        let bytes = encode_message(MessageKind::StateUpdate, &update)?;
        self.sender
            .send(Command::Send(bytes))
            .map_err(|_| StreamError::Disconnected)
    }
}

impl Drop for StreamServer {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
    }
}

fn worker_loop(listener: TcpListener, rx: Receiver<Command>, build_info: String) {
    let mut stream: Option<TcpStream> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(16)) {
            Ok(Command::Send(buffer)) => {
                if let Some(conn) = stream.as_mut() {
                    if let Err(err) = write_all(conn, &buffer) {
                        eprintln!("[grim_engine::stream] send failed: {err:?}; waiting for reconnect");
                        stream = None;
                    }
                }
            }
            Ok(Command::Shutdown) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if stream.is_none() {
            match listener.accept() {
                Ok((mut conn, addr)) => {
                    if let Err(err) = conn.set_nodelay(true) {
                        eprintln!(
                            "[grim_engine::stream] failed to configure connection from {addr}: {err:?}"
                        );
                        continue;
                    }
                    match send_hello(&mut conn, &build_info) {
                        Ok(()) => {
                            eprintln!(
                                "[grim_engine::stream] viewer connected from {addr}"
                            );
                            stream = Some(conn);
                        }
                        Err(err) => {
                            eprintln!(
                                "[grim_engine::stream] handshake error with {addr}: {err:?}"
                            );
                        }
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                Err(err) => {
                    eprintln!("[grim_engine::stream] accept error: {err:?}");
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }
}

fn send_hello(stream: &mut TcpStream, build_info: &str) -> Result<(), io::Error> {
    let hello = Hello::new("grim_engine", Some(build_info.to_string()));
    let message = encode_message(MessageKind::Hello, &hello)
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
    write_all(stream, &message)
}

fn write_all(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        match stream.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "remote closed connection",
                ))
            }
            Ok(written) => offset += written,
            Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}
