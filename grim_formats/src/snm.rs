use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use byteorder::{BigEndian, LittleEndian, ReadBytesExt};

/// FourCC helper constants.
const TAG_SANM: u32 = u32::from_be_bytes(*b"SANM");
const TAG_ANIM: u32 = u32::from_be_bytes(*b"ANIM");
const TAG_SHDR: u32 = u32::from_be_bytes(*b"SHDR");
const TAG_AHDR: u32 = u32::from_be_bytes(*b"AHDR");
const TAG_FLHD: u32 = u32::from_be_bytes(*b"FLHD");
const TAG_FRME: u32 = u32::from_be_bytes(*b"FRME");
const TAG_ANNO: u32 = u32::from_be_bytes(*b"ANNO");
const TAG_BL16: u32 = u32::from_be_bytes(*b"Bl16");
const TAG_WAVE: u32 = u32::from_be_bytes(*b"Wave");

/// Parsed representation of an SNM (Smush) cutscene file.
#[derive(Debug, Clone)]
pub struct SnmFile {
    pub source: Option<PathBuf>,
    pub header: SnmHeader,
    pub audio: Option<SnmAudioInfo>,
    pub frames: Vec<SnmFrame>,
}

/// High level metadata extracted from the `SHDR` chunk.
#[derive(Debug, Clone, Copy)]
pub struct SnmHeader {
    pub version: u16,
    pub frame_count: u32,
    pub width: u16,
    pub height: u16,
    pub frame_rate: u32,
    pub flags: u16,
}

/// Audio stream description pulled from the `FLHD` metadata.
#[derive(Debug, Clone, Copy)]
pub struct SnmAudioInfo {
    pub sample_rate: u32,
    pub channels: u32,
}

/// Raw per-frame payloads extracted from the container.
#[derive(Debug, Clone)]
pub struct SnmFrame {
    pub index: u32,
    pub blocky16: Option<Vec<u8>>,
    pub wave: Option<Vec<u8>>,
    pub extra_chunks: Vec<SnmSubChunk>,
}

/// Unhandled sub-chunk captured for inspection/debugging.
#[derive(Debug, Clone)]
pub struct SnmSubChunk {
    pub tag: [u8; 4],
    pub data: Vec<u8>,
}

impl SnmFile {
    /// Parse an SNM file from disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = std::fs::File::open(path)
            .with_context(|| format!("failed to open SNM file {}", path.display()))?;
        let reader = std::io::BufReader::new(file);
        let mut parsed = Self::read_from(reader)?;
        parsed.source = Some(path.to_path_buf());
        Ok(parsed)
    }

    /// Parse an SNM file from an arbitrary reader.
    pub fn read_from<R: Read + Seek>(mut reader: R) -> Result<Self> {
        let magic = reader
            .read_u32::<BigEndian>()
            .context("failed to read SNM magic")?;
        if magic != TAG_SANM && magic != TAG_ANIM {
            bail!("unsupported SNM magic {:08x}", magic);
        }

        let _file_size = reader
            .read_u32::<BigEndian>()
            .context("failed to read SNM file size")?;

        let header_tag = reader
            .read_u32::<BigEndian>()
            .context("failed to read header chunk tag")?;
        let header_size = reader
            .read_u32::<BigEndian>()
            .context("failed to read header chunk size")?;

        let header_start = reader
            .stream_position()
            .context("failed to capture header start")?;

        let header = match header_tag {
            TAG_SHDR => parse_shdr(&mut reader).context("failed to parse SHDR header")?,
            TAG_AHDR => bail!("demo SMUSH headers are not supported yet"),
            _ => bail!("unexpected header chunk {:08x}", header_tag),
        };

        // Align to even boundary as per SMUSH chunk rules.
        let mut next_chunk = header_start + header_size as u64;
        if header_size & 1 != 0 {
            next_chunk += 1;
        }
        reader
            .seek(SeekFrom::Start(next_chunk))
            .context("failed to seek past SHDR chunk")?;

        // Expect an `FLHD` chunk that describes the frame payloads.
        let flhd_tag = reader
            .read_u32::<BigEndian>()
            .context("failed to read FLHD tag")?;
        if flhd_tag != TAG_FLHD {
            bail!("expected FLHD chunk, found {:08x}", flhd_tag);
        }

        let flhd_size = reader
            .read_u32::<BigEndian>()
            .context("failed to read FLHD size")?;
        let mut flhd = vec![0u8; flhd_size as usize];
        reader
            .read_exact(&mut flhd)
            .context("failed to read FLHD payload")?;
        if flhd_size & 1 != 0 {
            reader.seek(SeekFrom::Current(1)).ok();
        }

        let audio = parse_flhd(&flhd).context("failed to parse FLHD metadata")?;

        let mut frames = Vec::new();
        let mut frame_index = 0u32;

        loop {
            let tag = match reader.read_u32::<BigEndian>() {
                Ok(val) => val,
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(err) => return Err(err).context("failed to read next chunk tag"),
            };

            let size = reader
                .read_u32::<BigEndian>()
                .context("failed to read chunk size")?;

            if tag == TAG_ANNO {
                // Skip announcement payloads for now.
                reader
                    .seek(SeekFrom::Current(size as i64 + i64::from(size & 1)))
                    .context("failed to skip ANNO chunk")?;
                continue;
            }

            if tag != TAG_FRME {
                bail!("unexpected chunk {:08x} in frame stream", tag);
            }

            let mut frame_data = vec![0u8; size as usize];
            reader
                .read_exact(&mut frame_data)
                .with_context(|| format!("failed to read FRME payload at frame {frame_index}"))?;

            if size & 1 != 0 {
                reader.seek(SeekFrom::Current(1)).ok();
            }

            let frame = parse_frame(frame_index, &frame_data)
                .with_context(|| format!("failed to parse frame payload {frame_index}"))?;
            frames.push(frame);
            frame_index += 1;
        }

        Ok(Self {
            source: None,
            header,
            audio,
            frames,
        })
    }
}

fn parse_shdr<R: Read>(reader: &mut R) -> Result<SnmHeader> {
    let version = reader
        .read_u16::<LittleEndian>()
        .context("failed to read SHDR version")?;
    let frame_count = reader
        .read_u32::<LittleEndian>()
        .context("failed to read SHDR frame count")?;
    let _unknown0 = reader
        .read_u16::<LittleEndian>()
        .context("failed to read SHDR reserved0")?;
    let width = reader
        .read_u16::<LittleEndian>()
        .context("failed to read SHDR width")?;
    let height = reader
        .read_u16::<LittleEndian>()
        .context("failed to read SHDR height")?;
    let _unknown1 = reader
        .read_u16::<LittleEndian>()
        .context("failed to read SHDR reserved1")?;
    let frame_rate = reader
        .read_u32::<LittleEndian>()
        .context("failed to read SHDR frame rate")?;
    let flags = reader
        .read_u16::<LittleEndian>()
        .context("failed to read SHDR flags")?;

    Ok(SnmHeader {
        version,
        frame_count,
        width,
        height,
        frame_rate,
        flags,
    })
}

fn parse_flhd(data: &[u8]) -> Result<Option<SnmAudioInfo>> {
    let mut cursor = std::io::Cursor::new(data);
    let mut audio_info: Option<SnmAudioInfo> = None;

    while (cursor.position() as usize) < data.len() {
        if data.len() - (cursor.position() as usize) < 8 {
            // Remaining padding or bookkeeping bytes that do not form a full chunk header.
            break;
        }

        let tag = cursor
            .read_u32::<BigEndian>()
            .context("failed to read FLHD sub-chunk tag")?;
        let sub_size = cursor
            .read_u32::<BigEndian>()
            .context("failed to read FLHD sub-chunk size")?;

        match tag {
            TAG_BL16 => {
                // Skip Blocky16 metadata for now.
                cursor
                    .seek(SeekFrom::Current(sub_size as i64 + i64::from(sub_size & 1)))
                    .context("failed to skip FLHD Bl16 payload")?;
            }
            TAG_WAVE => {
                match sub_size {
                    8 => {
                        let sample_rate = cursor
                            .read_u32::<LittleEndian>()
                            .context("failed to read wave sample rate")?;
                        let channels = cursor
                            .read_u32::<LittleEndian>()
                            .context("failed to read wave channel count")?;
                        audio_info = Some(SnmAudioInfo {
                            sample_rate,
                            channels,
                        });
                    }
                    s if s >= 16 => {
                        let codec = cursor
                            .read_u32::<BigEndian>()
                            .context("failed to read wave codec tag")?;
                        if codec == u32::from_be_bytes(*b"VIMA")
                            || codec == u32::from_be_bytes(*b"PSAD")
                        {
                            let sample_rate = cursor
                                .read_u32::<LittleEndian>()
                                .context("failed to read wave sample rate")?;
                            let channels = cursor
                                .read_u32::<LittleEndian>()
                                .context("failed to read wave channel count")?;
                            // Skip baseline block alignment + sample bits.
                            cursor
                                .seek(SeekFrom::Current(8))
                                .context("failed to skip wave block alignment")?;
                            audio_info = Some(SnmAudioInfo {
                                sample_rate,
                                channels,
                            });
                            let remainder = sub_size as i64 - 20;
                            if remainder > 0 {
                                cursor.seek(SeekFrom::Current(remainder)).ok();
                            }
                        } else {
                            cursor
                                .seek(SeekFrom::Current((sub_size as i64) - 4))
                                .context("failed to skip unknown wave codec payload")?;
                        }
                    }
                    _ => {
                        bail!("wave header too small ({sub_size} bytes)");
                    }
                }

                if sub_size & 1 != 0 {
                    cursor.seek(SeekFrom::Current(1)).ok();
                }
            }
            _ => {
                // Preserve unknown metadata for inspection.
                let mut buf = vec![0u8; sub_size as usize];
                cursor
                    .read_exact(&mut buf)
                    .with_context(|| format!("failed to read FLHD payload for {:08x}", tag))?;
                if sub_size & 1 != 0 {
                    cursor.seek(SeekFrom::Current(1)).ok();
                }
                let mut tag_bytes = [0u8; 4];
                tag_bytes.copy_from_slice(&tag.to_be_bytes());
                // We ignore the data but this surfaces in the diagnostic path.
                let _ = SnmSubChunk {
                    tag: tag_bytes,
                    data: buf,
                };
            }
        }
    }

    Ok(audio_info)
}

fn parse_frame(index: u32, data: &[u8]) -> Result<SnmFrame> {
    let mut cursor = std::io::Cursor::new(data);
    let mut blocky16 = None;
    let mut wave = None;
    let mut extra_chunks = Vec::new();

    while (cursor.position() as usize) < data.len() {
        if data.len() - (cursor.position() as usize) < 8 {
            break;
        }

        let tag = cursor
            .read_u32::<BigEndian>()
            .context("failed to read FRME sub-chunk tag")?;
        let sub_size = cursor
            .read_u32::<BigEndian>()
            .context("failed to read FRME sub-chunk size")?;
        let mut payload = vec![0u8; sub_size as usize];
        cursor
            .read_exact(&mut payload)
            .with_context(|| format!("failed to read frame {index} payload for {:08x}", tag))?;

        match tag {
            TAG_BL16 => blocky16 = Some(payload),
            TAG_WAVE => wave = Some(payload),
            _ => {
                let mut tag_bytes = [0u8; 4];
                tag_bytes.copy_from_slice(&tag.to_be_bytes());
                extra_chunks.push(SnmSubChunk {
                    tag: tag_bytes,
                    data: payload,
                });
            }
        }

        if sub_size & 1 != 0 {
            cursor.seek(SeekFrom::Current(1)).ok();
        }
    }

    Ok(SnmFrame {
        index,
        blocky16,
        wave,
        extra_chunks,
    })
}
