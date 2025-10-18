use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{self, RecvTimeoutError};
use grim_stream::{
    Control, Frame, HEADER_LEN, Hello, MessageHeader, MessageKind, MovieControl, MovieStart,
    PROTOCOL_VERSION, ProtocolError, StateUpdate, StreamConfig, Telemetry, TimelineMark,
    decode_payload, encode_message,
};
use thiserror::Error;

const RECONNECT_DELAY_MS: u64 = 750;

#[derive(Debug, Clone)]
pub enum RetailEvent {
    Connecting { addr: String, attempt: u32 },
    Connected(Hello),
    StreamConfig(StreamConfig),
    Frame(Frame),
    Timeline(TimelineMark),
    ProtocolError(String),
    Disconnected { reason: String },
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Connecting { addr: String, attempt: u32 },
    Connected(Hello),
    ViewerReady,
    State(StateUpdate),
    MovieStart(MovieStart),
    MovieControl(MovieControl),
    Timeline(TimelineMark),
    ProtocolError(String),
    Disconnected { reason: String },
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum EngineCommand {
    MovieControl(MovieControl),
}

pub type EngineCommandSender = crossbeam_channel::Sender<EngineCommand>;
type EngineCommandReceiver = crossbeam_channel::Receiver<EngineCommand>;

pub struct EngineClient {
    pub events: Receiver<EngineEvent>,
    pub commands: EngineCommandSender,
}

pub fn spawn_retail_client(addr: String) -> Receiver<RetailEvent> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("grim_stream_retail".to_string())
        .spawn(move || retail_loop(addr, tx))
        .expect("spawn retail stream thread");
    rx
}

pub fn spawn_engine_client(addr: String) -> EngineClient {
    let (tx, rx) = mpsc::channel();
    let (command_tx, command_rx) = crossbeam_channel::unbounded();
    thread::Builder::new()
        .name("grim_stream_engine".to_string())
        .spawn(move || engine_loop(addr, tx, command_rx))
        .expect("spawn engine stream thread");
    EngineClient {
        events: rx,
        commands: command_tx,
    }
}

fn retail_loop(addr: String, tx: Sender<RetailEvent>) {
    let mut attempt: u32 = 0;
    loop {
        attempt = attempt.wrapping_add(1);
        if tx
            .send(RetailEvent::Connecting {
                addr: addr.clone(),
                attempt,
            })
            .is_err()
        {
            break;
        }

        match TcpStream::connect(&addr) {
            Ok(mut stream) => {
                if let Err(err) = stream.set_nodelay(true) {
                    let _ = tx.send(RetailEvent::ProtocolError(format!(
                        "failed to enable TCP_NODELAY: {err}"
                    )));
                }
                if let Err(err) = retail_session(&mut stream, &tx) {
                    let _ = tx.send(RetailEvent::Disconnected {
                        reason: err.to_string(),
                    });
                }
            }
            Err(err) => {
                let _ = tx.send(RetailEvent::Disconnected {
                    reason: format!("connect error: {err}"),
                });
                thread::sleep(Duration::from_millis(RECONNECT_DELAY_MS));
            }
        }
    }
}

fn engine_loop(addr: String, tx: Sender<EngineEvent>, commands: EngineCommandReceiver) {
    let mut attempt: u32 = 0;
    loop {
        attempt = attempt.wrapping_add(1);
        if tx
            .send(EngineEvent::Connecting {
                addr: addr.clone(),
                attempt,
            })
            .is_err()
        {
            break;
        }

        match TcpStream::connect(&addr) {
            Ok(mut stream) => {
                if let Err(err) = stream.set_nodelay(true) {
                    let _ = tx.send(EngineEvent::ProtocolError(format!(
                        "failed to enable TCP_NODELAY: {err}"
                    )));
                }
                let connected = Arc::new(AtomicBool::new(true));
                let writer_handle = match stream.try_clone() {
                    Ok(writer_stream) => {
                        let command_rx = commands.clone();
                        let tx_clone = tx.clone();
                        let alive_flag = connected.clone();
                        Some(
                            thread::Builder::new()
                                .name("grim_stream_engine_tx".to_string())
                                .spawn(move || {
                                    engine_command_loop(
                                        writer_stream,
                                        command_rx,
                                        tx_clone,
                                        alive_flag,
                                    )
                                })
                                .expect("spawn engine stream tx thread"),
                        )
                    }
                    Err(err) => {
                        let _ = tx.send(EngineEvent::ProtocolError(format!(
                            "failed to clone engine stream for writer: {err}"
                        )));
                        None
                    }
                };
                if let Err(err) = engine_session(&mut stream, &tx) {
                    let _ = tx.send(EngineEvent::Disconnected {
                        reason: err.to_string(),
                    });
                }
                connected.store(false, Ordering::SeqCst);
                if let Some(handle) = writer_handle {
                    let _ = handle.join();
                }
            }
            Err(err) => {
                let _ = tx.send(EngineEvent::Disconnected {
                    reason: format!("connect error: {err}"),
                });
                thread::sleep(Duration::from_millis(RECONNECT_DELAY_MS));
            }
        }
    }
}

fn retail_session(stream: &mut TcpStream, tx: &Sender<RetailEvent>) -> Result<(), StreamReadError> {
    loop {
        let (header, payload) = read_message(stream)?;
        match header.kind {
            MessageKind::Hello => {
                let hello = decode_payload::<Hello>(&payload)?;
                if tx.send(RetailEvent::Connected(hello)).is_err() {
                    break;
                }
            }
            MessageKind::StreamConfig => {
                let config = decode_payload::<StreamConfig>(&payload)?;
                if tx.send(RetailEvent::StreamConfig(config)).is_err() {
                    break;
                }
            }
            MessageKind::Frame => {
                let frame = decode_payload::<Frame>(&payload)?;
                if tx.send(RetailEvent::Frame(frame)).is_err() {
                    break;
                }
            }
            MessageKind::TimelineMark => match decode_payload::<TimelineMark>(&payload) {
                Ok(mark) => {
                    if tx.send(RetailEvent::Timeline(mark)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = tx.send(RetailEvent::ProtocolError(format!(
                        "timeline decode error: {err}"
                    )));
                }
            },
            MessageKind::Telemetry => {
                let _ = decode_payload::<Telemetry>(&payload);
            }
            other => {
                let _ = tx.send(RetailEvent::ProtocolError(format!(
                    "ignored retail message kind {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn engine_session(stream: &mut TcpStream, tx: &Sender<EngineEvent>) -> Result<(), StreamReadError> {
    let mut sent_ready = false;
    loop {
        let (header, payload) = read_message(stream)?;
        match header.kind {
            MessageKind::Hello => {
                let hello = decode_payload::<Hello>(&payload)?;
                if tx.send(EngineEvent::Connected(hello)).is_err() {
                    break;
                }
                if !sent_ready {
                    if let Err(err) = send_viewer_ready(stream) {
                        let _ = tx.send(EngineEvent::ProtocolError(format!(
                            "failed to send viewer-ready control: {err}"
                        )));
                        return Err(StreamReadError::Io(err));
                    }
                    let _ = tx.send(EngineEvent::ViewerReady);
                    sent_ready = true;
                }
            }
            MessageKind::StateUpdate => {
                let update = decode_payload::<StateUpdate>(&payload)?;
                if tx.send(EngineEvent::State(update)).is_err() {
                    break;
                }
            }
            MessageKind::MovieStart => match decode_payload::<MovieStart>(&payload) {
                Ok(start) => {
                    if tx.send(EngineEvent::MovieStart(start)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = tx.send(EngineEvent::ProtocolError(format!(
                        "movie start decode error: {err}"
                    )));
                }
            },
            MessageKind::MovieControl => match decode_payload::<MovieControl>(&payload) {
                Ok(control) => {
                    if tx.send(EngineEvent::MovieControl(control)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = tx.send(EngineEvent::ProtocolError(format!(
                        "movie control decode error: {err}"
                    )));
                }
            },
            MessageKind::TimelineMark => match decode_payload::<TimelineMark>(&payload) {
                Ok(mark) => {
                    if tx.send(EngineEvent::Timeline(mark)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = tx.send(EngineEvent::ProtocolError(format!(
                        "timeline decode error: {err}"
                    )));
                }
            },
            other => {
                let _ = tx.send(EngineEvent::ProtocolError(format!(
                    "ignored engine message kind {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn read_message(stream: &mut TcpStream) -> Result<(MessageHeader, Vec<u8>), StreamReadError> {
    let mut header_bytes = [0u8; HEADER_LEN];
    stream.read_exact(&mut header_bytes)?;
    let header = MessageHeader::decode(&header_bytes)?;
    let mut payload = vec![0u8; header.length as usize];
    stream.read_exact(&mut payload)?;
    Ok((header, payload))
}

fn engine_command_loop(
    mut stream: TcpStream,
    commands: EngineCommandReceiver,
    tx: Sender<EngineEvent>,
    alive: Arc<AtomicBool>,
) {
    while alive.load(Ordering::SeqCst) {
        match commands.recv_timeout(Duration::from_millis(50)) {
            Ok(EngineCommand::MovieControl(control)) => {
                match encode_message(MessageKind::MovieControl, &control) {
                    Ok(bytes) => {
                        if let Err(err) = stream.write_all(&bytes) {
                            let _ = tx.send(EngineEvent::ProtocolError(format!(
                                "failed to send movie control: {err}"
                            )));
                            alive.store(false, Ordering::SeqCst);
                            break;
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(EngineEvent::ProtocolError(format!(
                            "movie control encode error: {err}"
                        )));
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[derive(Debug, Error)]
enum StreamReadError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}

fn send_viewer_ready(stream: &mut TcpStream) -> Result<(), std::io::Error> {
    let message = Control::ViewerReady {
        protocol: PROTOCOL_VERSION,
        features: Vec::new(),
    };
    let bytes = encode_message(MessageKind::Control, &message)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;
    stream.write_all(&bytes)
}
