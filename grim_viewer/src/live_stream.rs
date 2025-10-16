use std::io::Read;
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use grim_stream::{
    Frame, HEADER_LEN, Hello, MessageHeader, MessageKind, ProtocolError, StateUpdate, StreamConfig,
    Telemetry, decode_payload,
};
use thiserror::Error;

const RECONNECT_DELAY_MS: u64 = 750;

#[derive(Debug, Clone)]
pub enum RetailEvent {
    Connecting { addr: String, attempt: u32 },
    Connected(Hello),
    StreamConfig(StreamConfig),
    Frame(Frame),
    ProtocolError(String),
    Disconnected { reason: String },
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Connecting { addr: String, attempt: u32 },
    Connected(Hello),
    State(StateUpdate),
    ProtocolError(String),
    Disconnected { reason: String },
}

pub fn spawn_retail_client(addr: String) -> Receiver<RetailEvent> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("grim_stream_retail".to_string())
        .spawn(move || retail_loop(addr, tx))
        .expect("spawn retail stream thread");
    rx
}

pub fn spawn_engine_client(addr: String) -> Receiver<EngineEvent> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("grim_stream_engine".to_string())
        .spawn(move || engine_loop(addr, tx))
        .expect("spawn engine stream thread");
    rx
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

fn engine_loop(addr: String, tx: Sender<EngineEvent>) {
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
                if let Err(err) = engine_session(&mut stream, &tx) {
                    let _ = tx.send(EngineEvent::Disconnected {
                        reason: err.to_string(),
                    });
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
    loop {
        let (header, payload) = read_message(stream)?;
        match header.kind {
            MessageKind::Hello => {
                let hello = decode_payload::<Hello>(&payload)?;
                if tx.send(EngineEvent::Connected(hello)).is_err() {
                    break;
                }
            }
            MessageKind::StateUpdate => {
                let update = decode_payload::<StateUpdate>(&payload)?;
                if tx.send(EngineEvent::State(update)).is_err() {
                    break;
                }
            }
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

#[derive(Debug, Error)]
enum StreamReadError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}
