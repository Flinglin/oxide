use std::io::{BufRead, Read, Write};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    REMOTE_DESKTOP_MAX_FRAME_UPDATE_BATCH_REGIONS, RemoteDesktopClipboardData,
    RemoteDesktopClipboardFormat, RemoteDesktopFrame, RemoteDesktopFrameCompression,
    RemoteDesktopFrameFormat, RemoteDesktopFrameUpdate, RemoteDesktopFrameUpdateBatch,
    RemoteDesktopHelperEvent, RemoteDesktopHelperRequest, RemoteDesktopRect, RemoteDesktopSize,
};

const MAX_BINARY_PAYLOAD_LEN: usize = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RemoteDesktopJsonLineError {
    #[error("remote desktop helper line is empty")]
    EmptyLine,
    #[error("remote desktop helper line read failed: {0}")]
    ReadFailed(#[from] std::io::Error),
    #[error("remote desktop helper JSON failed: {0}")]
    JsonFailed(#[from] serde_json::Error),
    #[error("remote desktop helper binary payload is too large: {0} bytes")]
    BinaryPayloadTooLarge(usize),
    #[error("remote desktop helper frame update batch is invalid")]
    InvalidFrameUpdateBatch,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteDesktopBinaryFrameUpdateHeader {
    size: RemoteDesktopSize,
    rect: RemoteDesktopRect,
    format: RemoteDesktopFrameFormat,
    #[serde(default)]
    graphics_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace_id: Option<u64>,
    #[serde(default)]
    compression: RemoteDesktopFrameCompression,
    payload_len: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum RemoteDesktopBinaryEventHeader {
    FrameBinary {
        size: RemoteDesktopSize,
        format: RemoteDesktopFrameFormat,
        #[serde(default)]
        graphics_epoch: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<u64>,
        #[serde(default)]
        compression: RemoteDesktopFrameCompression,
        payload_len: usize,
    },
    FrameUpdateBinary {
        size: RemoteDesktopSize,
        rect: RemoteDesktopRect,
        format: RemoteDesktopFrameFormat,
        #[serde(default)]
        graphics_epoch: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<u64>,
        #[serde(default)]
        compression: RemoteDesktopFrameCompression,
        payload_len: usize,
    },
    FrameUpdateBatchBinary {
        updates: Vec<RemoteDesktopBinaryFrameUpdateHeader>,
        payload_len: usize,
    },
    ClipboardDataBinary {
        format: RemoteDesktopClipboardFormat,
        payload_len: usize,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum RemoteDesktopBinaryRequestHeader {
    ClipboardDataBinary {
        format: RemoteDesktopClipboardFormat,
        payload_len: usize,
    },
}

pub fn encode_request_line(
    request: &RemoteDesktopHelperRequest,
) -> Result<Zeroizing<String>, RemoteDesktopJsonLineError> {
    encode_line(request)
}

pub fn decode_request_line(
    line: &str,
) -> Result<RemoteDesktopHelperRequest, RemoteDesktopJsonLineError> {
    decode_line(line)
}

pub fn write_request_line(
    writer: &mut impl Write,
    request: &RemoteDesktopHelperRequest,
) -> Result<(), RemoteDesktopJsonLineError> {
    match request {
        RemoteDesktopHelperRequest::ClipboardData { data } => write_binary_request(
            writer,
            RemoteDesktopBinaryRequestHeader::ClipboardDataBinary {
                format: data.format,
                payload_len: data.bytes.len(),
            },
            &data.bytes,
        ),
        _ => write_line(writer, request),
    }
}

pub fn read_request_line(
    reader: &mut impl BufRead,
) -> Result<Option<RemoteDesktopHelperRequest>, RemoteDesktopJsonLineError> {
    // Requests can contain credentials during the authentication stage.
    let mut line = Zeroizing::new(String::new());
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let trimmed = trim_json_line(&line)?;
    if let Ok(header) = serde_json::from_str::<RemoteDesktopBinaryRequestHeader>(trimmed) {
        return read_binary_request(reader, header).map(Some);
    }
    decode_line(trimmed).map(Some)
}

pub fn encode_event_line(
    event: &RemoteDesktopHelperEvent,
) -> Result<Zeroizing<String>, RemoteDesktopJsonLineError> {
    encode_line(event)
}

pub fn decode_event_line(
    line: &str,
) -> Result<RemoteDesktopHelperEvent, RemoteDesktopJsonLineError> {
    decode_line(line)
}

pub fn write_event_line(
    writer: &mut impl Write,
    event: &RemoteDesktopHelperEvent,
) -> Result<(), RemoteDesktopJsonLineError> {
    match event {
        RemoteDesktopHelperEvent::Frame { frame } => write_binary_event(
            writer,
            RemoteDesktopBinaryEventHeader::FrameBinary {
                size: frame.size,
                format: frame.format,
                graphics_epoch: frame.graphics_epoch,
                trace_id: frame.trace_id,
                compression: RemoteDesktopFrameCompression::None,
                payload_len: frame.bytes.len(),
            },
            &frame.bytes,
        ),
        RemoteDesktopHelperEvent::FrameUpdate { update } => write_binary_event(
            writer,
            RemoteDesktopBinaryEventHeader::FrameUpdateBinary {
                size: update.size,
                rect: update.rect,
                format: update.format,
                graphics_epoch: update.graphics_epoch,
                trace_id: update.trace_id,
                compression: update.compression,
                payload_len: update.bytes.len(),
            },
            &update.bytes,
        ),
        RemoteDesktopHelperEvent::FrameUpdateBatch { batch } => {
            write_binary_frame_update_batch(writer, batch)
        }
        RemoteDesktopHelperEvent::ClipboardData { data } => write_binary_event(
            writer,
            RemoteDesktopBinaryEventHeader::ClipboardDataBinary {
                format: data.format,
                payload_len: data.bytes.len(),
            },
            &data.bytes,
        ),
        _ => write_line(writer, event),
    }
}

pub fn read_event_line(
    reader: &mut impl BufRead,
) -> Result<Option<RemoteDesktopHelperEvent>, RemoteDesktopJsonLineError> {
    // Clipboard text can contain user secrets even when protocol events do not.
    let mut line = Zeroizing::new(String::new());
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let trimmed = trim_json_line(&line)?;
    if let Ok(header) = serde_json::from_str::<RemoteDesktopBinaryEventHeader>(trimmed) {
        return read_binary_event(reader, header).map(Some);
    }
    decode_line(trimmed).map(Some)
}

fn encode_line<T: serde::Serialize>(
    value: &T,
) -> Result<Zeroizing<String>, RemoteDesktopJsonLineError> {
    // Take ownership of serde's allocation immediately so secret-bearing JSON
    // is cleared on success and every subsequent I/O error path.
    let mut line = Zeroizing::new(serde_json::to_string(value)?);
    line.push('\n');
    Ok(line)
}

fn decode_line<T: serde::de::DeserializeOwned>(
    line: &str,
) -> Result<T, RemoteDesktopJsonLineError> {
    let trimmed = trim_json_line(line)?;
    Ok(serde_json::from_str(trimmed)?)
}

fn write_line<T: serde::Serialize>(
    writer: &mut impl Write,
    value: &T,
) -> Result<(), RemoteDesktopJsonLineError> {
    writer.write_all(encode_line(value)?.as_bytes())?;
    writer.flush()?;
    Ok(())
}

fn trim_json_line(line: &str) -> Result<&str, RemoteDesktopJsonLineError> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.trim().is_empty() {
        return Err(RemoteDesktopJsonLineError::EmptyLine);
    }
    Ok(trimmed)
}

fn write_binary_event(
    writer: &mut impl Write,
    header: RemoteDesktopBinaryEventHeader,
    payload: &[u8],
) -> Result<(), RemoteDesktopJsonLineError> {
    writer.write_all(encode_line(&header)?.as_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

fn write_binary_request(
    writer: &mut impl Write,
    header: RemoteDesktopBinaryRequestHeader,
    payload: &[u8],
) -> Result<(), RemoteDesktopJsonLineError> {
    writer.write_all(encode_line(&header)?.as_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

fn write_binary_frame_update_batch(
    writer: &mut impl Write,
    batch: &RemoteDesktopFrameUpdateBatch,
) -> Result<(), RemoteDesktopJsonLineError> {
    if !batch.is_complete() || batch.updates.len() > REMOTE_DESKTOP_MAX_FRAME_UPDATE_BATCH_REGIONS {
        return Err(RemoteDesktopJsonLineError::InvalidFrameUpdateBatch);
    }
    let payload_len = batch.byte_len();
    if payload_len > MAX_BINARY_PAYLOAD_LEN {
        return Err(RemoteDesktopJsonLineError::BinaryPayloadTooLarge(
            payload_len,
        ));
    }
    let updates = batch
        .updates
        .iter()
        .map(|update| RemoteDesktopBinaryFrameUpdateHeader {
            size: update.size,
            rect: update.rect,
            format: update.format,
            graphics_epoch: update.graphics_epoch,
            trace_id: update.trace_id,
            compression: update.compression,
            payload_len: update.bytes.len(),
        })
        .collect();
    let header = RemoteDesktopBinaryEventHeader::FrameUpdateBatchBinary {
        updates,
        payload_len,
    };
    writer.write_all(encode_line(&header)?.as_bytes())?;
    for update in &batch.updates {
        writer.write_all(&update.bytes)?;
    }
    writer.flush()?;
    Ok(())
}

fn read_binary_event(
    reader: &mut impl Read,
    header: RemoteDesktopBinaryEventHeader,
) -> Result<RemoteDesktopHelperEvent, RemoteDesktopJsonLineError> {
    let header = match header {
        RemoteDesktopBinaryEventHeader::FrameUpdateBatchBinary {
            updates,
            payload_len,
        } => return read_binary_frame_update_batch(reader, updates, payload_len),
        header => header,
    };
    let payload_len = match &header {
        RemoteDesktopBinaryEventHeader::FrameBinary { payload_len, .. }
        | RemoteDesktopBinaryEventHeader::FrameUpdateBinary { payload_len, .. }
        | RemoteDesktopBinaryEventHeader::ClipboardDataBinary { payload_len, .. } => *payload_len,
        RemoteDesktopBinaryEventHeader::FrameUpdateBatchBinary { .. } => unreachable!(),
    };
    if payload_len > MAX_BINARY_PAYLOAD_LEN {
        return Err(RemoteDesktopJsonLineError::BinaryPayloadTooLarge(
            payload_len,
        ));
    }
    let mut payload = vec![0; payload_len];
    reader.read_exact(&mut payload)?;
    Ok(match header {
        RemoteDesktopBinaryEventHeader::FrameBinary {
            size,
            format,
            graphics_epoch,
            trace_id,
            compression: RemoteDesktopFrameCompression::None,
            ..
        } => {
            let frame =
                RemoteDesktopFrame::new(size, format, payload).with_graphics_epoch(graphics_epoch);
            RemoteDesktopHelperEvent::Frame {
                frame: match trace_id {
                    Some(trace_id) => frame.with_trace_id(trace_id),
                    None => frame,
                },
            }
        }
        RemoteDesktopBinaryEventHeader::FrameUpdateBinary {
            size,
            rect,
            format,
            graphics_epoch,
            trace_id,
            compression: RemoteDesktopFrameCompression::None,
            ..
        } => {
            let update = RemoteDesktopFrameUpdate::new(size, rect, format, payload)
                .with_graphics_epoch(graphics_epoch);
            RemoteDesktopHelperEvent::FrameUpdate {
                update: match trace_id {
                    Some(trace_id) => update.with_trace_id(trace_id),
                    None => update,
                },
            }
        }
        RemoteDesktopBinaryEventHeader::FrameUpdateBatchBinary { .. } => unreachable!(),
        RemoteDesktopBinaryEventHeader::ClipboardDataBinary { format, .. } => {
            RemoteDesktopHelperEvent::ClipboardData {
                data: RemoteDesktopClipboardData::new(format, payload),
            }
        }
    })
}

fn read_binary_frame_update_batch(
    reader: &mut impl Read,
    updates: Vec<RemoteDesktopBinaryFrameUpdateHeader>,
    payload_len: usize,
) -> Result<RemoteDesktopHelperEvent, RemoteDesktopJsonLineError> {
    if updates.is_empty() || updates.len() > REMOTE_DESKTOP_MAX_FRAME_UPDATE_BATCH_REGIONS {
        return Err(RemoteDesktopJsonLineError::InvalidFrameUpdateBatch);
    }
    let expected_payload_len = updates
        .iter()
        .map(|update| update.payload_len)
        .try_fold(0_usize, usize::checked_add)
        .ok_or(RemoteDesktopJsonLineError::InvalidFrameUpdateBatch)?;
    if payload_len != expected_payload_len {
        return Err(RemoteDesktopJsonLineError::InvalidFrameUpdateBatch);
    }
    if payload_len > MAX_BINARY_PAYLOAD_LEN {
        return Err(RemoteDesktopJsonLineError::BinaryPayloadTooLarge(
            payload_len,
        ));
    }

    // Read each region into its final allocation to avoid holding a second
    // batch-sized payload buffer while the UI is under graphics pressure.
    let mut decoded_updates = Vec::with_capacity(updates.len());
    for update in updates {
        let mut bytes = vec![0; update.payload_len];
        reader.read_exact(&mut bytes)?;
        let decoded_update =
            RemoteDesktopFrameUpdate::new(update.size, update.rect, update.format, bytes)
                .with_graphics_epoch(update.graphics_epoch);
        decoded_updates.push(match update.trace_id {
            Some(trace_id) => decoded_update.with_trace_id(trace_id),
            None => decoded_update,
        });
    }
    let batch = RemoteDesktopFrameUpdateBatch::new(decoded_updates);
    if !batch.is_complete() {
        return Err(RemoteDesktopJsonLineError::InvalidFrameUpdateBatch);
    }
    Ok(RemoteDesktopHelperEvent::FrameUpdateBatch { batch })
}

fn read_binary_request(
    reader: &mut impl Read,
    header: RemoteDesktopBinaryRequestHeader,
) -> Result<RemoteDesktopHelperRequest, RemoteDesktopJsonLineError> {
    let payload_len = match &header {
        RemoteDesktopBinaryRequestHeader::ClipboardDataBinary { payload_len, .. } => *payload_len,
    };
    if payload_len > MAX_BINARY_PAYLOAD_LEN {
        return Err(RemoteDesktopJsonLineError::BinaryPayloadTooLarge(
            payload_len,
        ));
    }
    let mut payload = vec![0; payload_len];
    reader.read_exact(&mut payload)?;
    Ok(match header {
        RemoteDesktopBinaryRequestHeader::ClipboardDataBinary { format, .. } => {
            RemoteDesktopHelperRequest::ClipboardData {
                data: RemoteDesktopClipboardData::new(format, payload),
            }
        }
    })
}
