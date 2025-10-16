//! Shared GrimStream protocol helpers.
//!
//! The protocol sends a fixed-size header followed by a MessagePack payload.
//! This crate keeps the framing logic in one place so both producers and
//! consumers stay interoperable.

use std::convert::TryFrom;

use bytes::Buf;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_repr::{Deserialize_repr, Serialize_repr};
use thiserror::Error;

/// Bytes that prefix every GrimStream message ("GRIM").
pub const HEADER_MAGIC: [u8; 4] = *b"GRIM";

/// Protocol revision understood by this crate.
pub const PROTOCOL_VERSION: u16 = 0x0001;

/// Length of the binary header in bytes.
pub const HEADER_LEN: usize = 4 + 2 + 2 + 4;

/// Message kinds understood by GrimStream v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr, Hash)]
#[repr(u16)]
pub enum MessageKind {
    Hello = 0x0001,
    StreamConfig = 0x0002,
    Frame = 0x0003,
    Telemetry = 0x0004,
    StateUpdate = 0x0005,
    TimelineMark = 0x0006,
    Control = 0x0007,
    Heartbeat = 0x0008,
}

/// Envelope describing the upcoming payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageHeader {
    pub version: u16,
    pub kind: MessageKind,
    pub length: u32,
}

impl MessageHeader {
    /// Encode the header as big-endian bytes.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[..4].copy_from_slice(&HEADER_MAGIC);
        out[4..6].copy_from_slice(&self.version.to_be_bytes());
        out[6..8].copy_from_slice(&(self.kind as u16).to_be_bytes());
        out[8..12].copy_from_slice(&self.length.to_be_bytes());
        out
    }

    /// Decode a header from raw bytes.
    pub fn decode(input: &[u8]) -> Result<Self, ProtocolError> {
        if input.len() < HEADER_LEN {
            return Err(ProtocolError::TruncatedHeader);
        }
        if &input[..4] != HEADER_MAGIC {
            return Err(ProtocolError::BadMagic);
        }
        let mut version_bytes = &input[4..6];
        let version = version_bytes.get_u16();
        let mut kind_bytes = &input[6..8];
        let kind_raw = kind_bytes.get_u16();
        let kind = MessageKind::try_from(kind_raw)
            .map_err(|_| ProtocolError::UnknownMessageKind(kind_raw))?;
        let mut len_bytes = &input[8..12];
        let length = len_bytes.get_u32();
        Ok(Self {
            version,
            kind,
            length,
        })
    }
}

impl TryFrom<u16> for MessageKind {
    type Error = ();

    fn try_from(value: u16) -> std::result::Result<Self, Self::Error> {
        match value {
            0x0001 => Ok(Self::Hello),
            0x0002 => Ok(Self::StreamConfig),
            0x0003 => Ok(Self::Frame),
            0x0004 => Ok(Self::Telemetry),
            0x0005 => Ok(Self::StateUpdate),
            0x0006 => Ok(Self::TimelineMark),
            0x0007 => Ok(Self::Control),
            0x0008 => Ok(Self::Heartbeat),
            _ => Err(()),
        }
    }
}

/// Minimal handshake message that opens a stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: String,
    pub producer: String,
    pub build: Option<String>,
}

impl Hello {
    pub fn new(producer: impl Into<String>, build: Option<String>) -> Self {
        Self {
            protocol: "GrimStream".to_string(),
            producer: producer.into(),
            build,
        }
    }
}

/// Describes the raw framebuffer stream coming from the retail capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamConfig {
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
    pub stride_bytes: u32,
    pub nominal_fps: Option<f32>,
}

/// Supported pixel layouts for a framebuffer message.
#[derive(Debug, Clone, Copy, Serialize_repr, Deserialize_repr, PartialEq, Eq)]
#[repr(u16)]
pub enum PixelFormat {
    Rgba8 = 0x0001,
}

/// Raw frame data emitted by the retail capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub frame_id: u64,
    pub host_time_ns: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry_time_ns: Option<u64>,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Retail telemetry payload mirrored from telemetry_events.jsonl.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Telemetry {
    pub seq: u64,
    pub label: String,
    #[serde(default)]
    pub timestamp: Option<u64>,
    #[serde(default)]
    pub data: Value,
}

/// Coverage counter delta or snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageCounter {
    pub key: String,
    pub value: u64,
}

fn vec_is_empty<T>(vec: &Vec<T>) -> bool {
    vec.is_empty()
}

/// Run-time state delta published by the Rust host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateUpdate {
    pub seq: u64,
    pub host_time_ns: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_setup: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_hotspot: Option<String>,
    #[serde(skip_serializing_if = "vec_is_empty", default)]
    pub coverage: Vec<CoverageCounter>,
    #[serde(skip_serializing_if = "vec_is_empty", default)]
    pub events: Vec<String>,
}

/// Error conditions returned by the protocol helpers.
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("header smaller than {HEADER_LEN} bytes")]
    TruncatedHeader,
    #[error("header magic mismatch")]
    BadMagic,
    #[error("message kind {0:#06x} is unknown")]
    UnknownMessageKind(u16),
    #[error("payload length mismatch: header declared {expected} bytes but read {actual}")]
    LengthMismatch { expected: u32, actual: usize },
    #[error("payload decode error: {0}")]
    PayloadDecode(#[from] rmp_serde::decode::Error),
    #[error("payload encode error: {0}")]
    PayloadEncode(#[from] rmp_serde::encode::Error),
}

/// Wraps a payload with framing suitable for the wire.
pub fn encode_message<T>(kind: MessageKind, payload: &T) -> Result<Vec<u8>, ProtocolError>
where
    T: Serialize,
{
    let payload_bytes = rmp_serde::to_vec_named(payload)?;
    let header = MessageHeader {
        version: PROTOCOL_VERSION,
        kind,
        length: u32::try_from(payload_bytes.len()).map_err(|_| ProtocolError::LengthMismatch {
            expected: u32::MAX,
            actual: payload_bytes.len(),
        })?,
    };
    let mut out = Vec::with_capacity(HEADER_LEN + payload_bytes.len());
    out.extend_from_slice(&header.encode());
    out.extend_from_slice(&payload_bytes);
    Ok(out)
}

/// Decodes a framed message returning both header and payload bytes.
pub fn decode_envelope(bytes: &[u8]) -> std::result::Result<(MessageHeader, &[u8]), ProtocolError> {
    if bytes.len() < HEADER_LEN {
        return Err(ProtocolError::TruncatedHeader);
    }
    let header = MessageHeader::decode(&bytes[..HEADER_LEN])?;
    let payload = &bytes[HEADER_LEN..];
    if payload.len() != header.length as usize {
        return Err(ProtocolError::LengthMismatch {
            expected: header.length,
            actual: payload.len(),
        });
    }
    Ok((header, payload))
}

/// Decode a payload straight into the requested type.
pub fn decode_payload<T>(payload: &[u8]) -> std::result::Result<T, ProtocolError>
where
    T: for<'de> Deserialize<'de>,
{
    let value = rmp_serde::from_slice(payload)?;
    Ok(value)
}
