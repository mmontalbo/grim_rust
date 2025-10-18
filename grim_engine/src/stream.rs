use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use crossbeam_channel::{self, Receiver as ControlReceiver, Sender as ControlSender};
use grim_stream::{
    decode_payload, encode_message, Control, Hello, MessageHeader, MessageKind, MovieControl,
    MovieStart, StateUpdate, HEADER_LEN,
};
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

#[allow(dead_code)]
#[derive(Debug, Clone)]
/// Control-plane movie response tagged with the connection generation.
pub struct MovieControlEvent {
    pub generation: u64,
    pub control: MovieControl,
}

#[allow(dead_code)]
#[derive(Clone)]
/// Convenience wrapper for awaiting movie control responses from the viewer.
pub struct MovieControlEvents {
    receiver: ControlReceiver<MovieControlEvent>,
}

#[allow(dead_code)]
impl MovieControlEvents {
    pub fn try_recv(&self) -> Result<MovieControlEvent, crossbeam_channel::TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn recv(&self) -> Result<MovieControlEvent, crossbeam_channel::RecvError> {
        self.receiver.recv()
    }

    pub fn receiver(&self) -> ControlReceiver<MovieControlEvent> {
        self.receiver.clone()
    }
}

/// Broadcasts GrimStream messages to a single connected subscriber.
pub struct StreamServer {
    sender: Sender<Command>,
    start: Instant,
    seq: AtomicU64,
    state: Arc<ConnectionState>,
    movie_controls: ControlReceiver<MovieControlEvent>,
}

impl StreamServer {
    pub fn bind<A: ToSocketAddrs>(addr: A, build: Option<String>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(addr).context("binding stream socket")?;
        listener
            .set_nonblocking(true)
            .context("setting stream listener non-blocking")?;
        let (tx, rx) = mpsc::channel();
        let build_info = build.unwrap_or_else(|| "dev".to_string());
        let state = Arc::new(ConnectionState::new());
        let (movie_tx, movie_rx) = crossbeam_channel::unbounded();
        thread::Builder::new()
            .name("grim_stream".to_string())
            .spawn({
                let state = state.clone();
                move || worker_loop(listener, rx, build_info, state, movie_tx)
            })
            .context("spawning stream worker thread")?;
        Ok(Self {
            sender: tx,
            start: Instant::now(),
            seq: AtomicU64::new(0),
            state,
            movie_controls: movie_rx,
        })
    }

    pub fn send_state_update(&self, mut update: StateUpdate) -> Result<(), StreamError> {
        if !self.state.is_ready() {
            return Ok(());
        }
        update.seq = self.seq.fetch_add(1, Ordering::Relaxed);
        if update.host_time_ns == 0 {
            update.host_time_ns = self.start.elapsed().as_nanos() as u64;
        }
        let bytes = encode_message(MessageKind::StateUpdate, &update)?;
        self.sender
            .send(Command::Send(bytes))
            .map_err(|_| StreamError::Disconnected)
    }

    #[allow(dead_code)]
    /// Publish a `MovieStart` announcement when the viewer is ready.
    pub fn send_movie_start(&self, start: MovieStart) -> Result<(), StreamError> {
        if !self.state.is_ready() {
            return Ok(());
        }
        let bytes = encode_message(MessageKind::MovieStart, &start)?;
        self.sender
            .send(Command::Send(bytes))
            .map_err(|_| StreamError::Disconnected)
    }

    pub fn viewer_gate(&self) -> StreamViewerGate {
        StreamViewerGate {
            state: self.state.clone(),
        }
    }

    #[allow(dead_code)]
    /// Obtain a handle for consuming `MovieControl` responses from the viewer.
    pub fn movie_controls(&self) -> MovieControlEvents {
        MovieControlEvents {
            receiver: self.movie_controls.clone(),
        }
    }

    #[allow(dead_code)]
    /// Current connection generation (increments each time the viewer reconnects).
    pub fn current_generation(&self) -> u64 {
        self.state.generation()
    }
}

impl Drop for StreamServer {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
    }
}

fn worker_loop(
    listener: TcpListener,
    rx: Receiver<Command>,
    build_info: String,
    state: Arc<ConnectionState>,
    movie_tx: ControlSender<MovieControlEvent>,
) {
    let mut stream: Option<TcpStream> = None;
    let mut control_worker: Option<thread::JoinHandle<()>> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(16)) {
            Ok(Command::Send(buffer)) => {
                if let Some(conn) = stream.as_mut() {
                    if let Err(err) = write_all(conn, &buffer) {
                        eprintln!(
                            "[grim_engine::stream] send failed: {err:?}; waiting for reconnect"
                        );
                        state.on_disconnect();
                        if let Some(handle) = control_worker.take() {
                            let _ = handle.join();
                        }
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
                            eprintln!("[grim_engine::stream] viewer connected from {addr}");
                            state.on_connect();
                            if let Some(handle) = control_worker.take() {
                                let _ = handle.join();
                            }
                            match conn.try_clone() {
                                Ok(clone) => {
                                    let state_clone = state.clone();
                                    let control_tx = movie_tx.clone();
                                    match thread::Builder::new()
                                        .name("grim_stream_ctrl".to_string())
                                        .spawn(move || control_loop(clone, state_clone, control_tx))
                                    {
                                        Ok(handle) => {
                                            control_worker = Some(handle);
                                            stream = Some(conn);
                                        }
                                        Err(err) => {
                                            eprintln!(
                                                "[grim_engine::stream] failed to spawn control loop: {err:?}"
                                            );
                                            state.on_disconnect();
                                        }
                                    }
                                }
                                Err(err) => {
                                    eprintln!(
                                        "[grim_engine::stream] failed to clone connection for control: {err:?}"
                                    );
                                    state.on_disconnect();
                                }
                            }
                        }
                        Err(err) => {
                            eprintln!("[grim_engine::stream] handshake error with {addr}: {err:?}");
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

    if let Some(handle) = control_worker {
        let _ = handle.join();
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

fn control_loop(
    mut stream: TcpStream,
    state: Arc<ConnectionState>,
    movie_tx: ControlSender<MovieControlEvent>,
) {
    let mut header_bytes = [0u8; HEADER_LEN];
    let generation = state.generation();
    loop {
        if let Err(err) = stream.read_exact(&mut header_bytes) {
            if err.kind() != io::ErrorKind::UnexpectedEof {
                eprintln!(
                    "[grim_engine::stream] control read error: {err:?}; resetting viewer gate"
                );
            }
            state.on_disconnect();
            break;
        }
        let header = match MessageHeader::decode(&header_bytes) {
            Ok(header) => header,
            Err(err) => {
                eprintln!("[grim_engine::stream] control header decode failed: {err:?}; skipping");
                continue;
            }
        };
        let mut payload = vec![0u8; header.length as usize];
        if let Err(err) = stream.read_exact(&mut payload) {
            eprintln!(
                "[grim_engine::stream] control payload read error: {err:?}; resetting viewer gate"
            );
            state.on_disconnect();
            break;
        }
        match header.kind {
            MessageKind::Control => match decode_payload::<Control>(&payload) {
                Ok(Control::ViewerReady { protocol, features }) => {
                    state.on_ready();
                    eprintln!(
                        "[grim_engine::stream] viewer ready (protocol={protocol:#06x}, features={:?})",
                        features
                    );
                }
                Err(err) => {
                    eprintln!("[grim_engine::stream] control payload decode failed: {err:?}");
                }
            },
            MessageKind::Heartbeat => {}
            MessageKind::MovieControl => match decode_payload::<MovieControl>(&payload) {
                Ok(control) => {
                    let event = MovieControlEvent {
                        generation,
                        control,
                    };
                    if let Err(err) = movie_tx.send(event) {
                        eprintln!(
                            "[grim_engine::stream] dropping movie control event: send failed: {err:?}"
                        );
                    }
                }
                Err(err) => {
                    eprintln!(
                        "[grim_engine::stream] movie control decode failed: {err:?}; skipping"
                    );
                }
            },
            other => {
                eprintln!(
                    "[grim_engine::stream] ignoring inbound message kind {other:?} on control plane"
                );
            }
        }
    }
}

struct ConnectionState {
    inner: Mutex<ConnectionInner>,
    cv: Condvar,
    ready: AtomicBool,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            inner: Mutex::new(ConnectionInner {
                connected: false,
                viewer_ready: false,
                generation: 0,
            }),
            cv: Condvar::new(),
            ready: AtomicBool::new(false),
        }
    }

    fn on_connect(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.connected = true;
        inner.viewer_ready = false;
        inner.generation = inner.generation.wrapping_add(1);
        self.ready.store(false, Ordering::SeqCst);
        self.cv.notify_all();
    }

    fn on_disconnect(&self) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.connected && !inner.viewer_ready {
            return;
        }
        inner.connected = false;
        inner.viewer_ready = false;
        self.ready.store(false, Ordering::SeqCst);
        self.cv.notify_all();
    }

    fn on_ready(&self) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.connected {
            return;
        }
        inner.viewer_ready = true;
        self.ready.store(true, Ordering::SeqCst);
        self.cv.notify_all();
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    fn generation(&self) -> u64 {
        self.inner.lock().unwrap().generation
    }
}

struct ConnectionInner {
    connected: bool,
    viewer_ready: bool,
    generation: u64,
}

#[derive(Clone)]
pub struct StreamViewerGate {
    state: Arc<ConnectionState>,
}

impl StreamViewerGate {
    pub fn wait_for_ready(&self) {
        let mut guard = self.state.inner.lock().unwrap();
        let start_generation = guard.generation;
        while !(guard.connected && guard.viewer_ready && guard.generation >= start_generation) {
            guard = self.state.cv.wait(guard).unwrap();
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state.is_ready()
    }
}
