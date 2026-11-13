//! OMP protocol-v2 transport reassembly, before logical records reach tape.

use std::borrow::Cow;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;

/// Largest physical RPC frame, including its newline.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Largest logical RPC record, excluding its tape delimiter.
pub const MAX_REASSEMBLED_BYTES: usize = 64 * 1024 * 1024;
const CHUNK_BYTES: usize = 256 * 1024;
const MAX_CHUNKS: usize = MAX_REASSEMBLED_BYTES / CHUNK_BYTES;
const MAX_CHUNK_ID_UTF16_UNITS: usize = 128;

/// One ordered logical record in flight; other records cannot interleave it.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    pending: Option<Pending>,
}

#[derive(Debug)]
struct Pending {
    id: String,
    count: usize,
    length: usize,
    next: usize,
    bytes: Vec<u8>,
}

#[derive(Deserialize)]
struct Kind<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Chunk<'a> {
    #[serde(borrow)]
    chunk_id: Cow<'a, str>,
    index: usize,
    count: usize,
    byte_length: usize,
    #[serde(borrow)]
    data: Cow<'a, str>,
}

/// A transport refusal. The caller must terminate capture after an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameError(&'static str);

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for FrameError {}

impl FrameDecoder {
    /// Admit one physical record. Unchunked records borrow the original bytes;
    /// a completed assembly owns its original JSON plus one tape newline.
    /// Non-transport syntax errors remain the logical decoder's residue.
    ///
    /// # Errors
    ///
    /// Refuses malformed, interrupted, out-of-order or oversized assemblies.
    pub fn push<'a>(&mut self, bytes: &'a [u8]) -> Result<Option<Cow<'a, [u8]>>, FrameError> {
        let is_chunk =
            serde_json::from_slice::<Kind<'_>>(bytes).is_ok_and(|frame| frame.kind == "rpc_chunk");
        if !is_chunk {
            self.finish()?;
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(FrameError("OMP RPC chunk exceeds physical frame limit"));
        }
        let chunk: Chunk<'_> = serde_json::from_slice(bytes)
            .map_err(|_| FrameError("invalid OMP RPC chunk fields"))?;
        chunk.validate()?;
        let payload_length = chunk.payload_length()?;
        if let Some(pending) = &self.pending
            && pending.id != chunk.chunk_id
        {
            return Err(FrameError("OMP RPC chunk sequence identity mismatch"));
        }
        if self.pending.is_none() {
            if chunk.index != 0 {
                return Err(FrameError(
                    "OMP RPC chunk sequence must start at index zero",
                ));
            }
            self.pending = Some(Pending {
                id: chunk.chunk_id.into_owned(),
                count: chunk.count,
                length: chunk.byte_length,
                next: 0,
                bytes: Vec::with_capacity(chunk.byte_length + 1),
            });
        }
        let pending = self
            .pending
            .as_mut()
            .expect("the first chunk starts an assembly");
        if pending.count != chunk.count
            || pending.length != chunk.byte_length
            || pending.next != chunk.index
        {
            return Err(FrameError("OMP RPC chunk sequence metadata mismatch"));
        }
        let start = pending.bytes.len();
        let end = start + payload_length;
        if end > pending.length {
            return Err(FrameError("OMP RPC chunks exceed declared length"));
        }
        pending.bytes.resize(end, 0);
        STANDARD
            .decode_slice(chunk.data.as_bytes(), &mut pending.bytes[start..])
            .map_err(|_| FrameError("OMP RPC chunk data is not canonical base64"))?;
        pending.next += 1;
        if pending.next < pending.count {
            return Ok(None);
        }
        if end != pending.length {
            return Err(FrameError("OMP RPC chunk sequence length mismatch"));
        }
        let mut pending = self
            .pending
            .take()
            .expect("the final chunk completes an assembly");
        validate_record(&pending.bytes)?;
        pending.bytes.push(b'\n');
        Ok(Some(Cow::Owned(pending.bytes)))
    }

    /// Close the selected stream after every physical record was delivered.
    ///
    /// # Errors
    ///
    /// Refuses EOF or an intervening record while an assembly is incomplete.
    pub fn finish(&self) -> Result<(), FrameError> {
        if self.pending.is_some() {
            return Err(FrameError(
                "OMP RPC stream ended or interrupted an incomplete chunk sequence",
            ));
        }
        Ok(())
    }
}

impl Chunk<'_> {
    fn validate(&self) -> Result<(), FrameError> {
        if self.chunk_id.is_empty()
            || self.chunk_id.encode_utf16().count() > MAX_CHUNK_ID_UTF16_UNITS
            || !(2..=MAX_CHUNKS).contains(&self.count)
            || self.index >= self.count
            || !(MAX_FRAME_BYTES..=MAX_REASSEMBLED_BYTES).contains(&self.byte_length)
        {
            return Err(FrameError("invalid OMP RPC chunk metadata"));
        }
        Ok(())
    }

    fn payload_length(&self) -> Result<usize, FrameError> {
        if self.data.is_empty() || !self.data.len().is_multiple_of(4) {
            return Err(FrameError("OMP RPC chunk data is not canonical base64"));
        }
        let padding = if self.data.ends_with("==") {
            2
        } else {
            usize::from(self.data.ends_with('='))
        };
        let length = self.data.len() / 4 * 3 - padding;
        if length > CHUNK_BYTES {
            return Err(FrameError("OMP RPC chunk payload exceeds transport limit"));
        }
        Ok(length)
    }
}

fn validate_record(bytes: &[u8]) -> Result<(), FrameError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| FrameError("OMP RPC assembled record is not UTF-8"))?;
    if !text.trim_start().starts_with('{') || bytes.contains(&b'\n') {
        return Err(FrameError(
            "OMP RPC assembled record must be one JSONL object",
        ));
    }
    serde_json::from_str::<serde::de::IgnoredAny>(text)
        .map_err(|_| FrameError("OMP RPC assembled record is not JSON"))?;
    Ok(())
}
