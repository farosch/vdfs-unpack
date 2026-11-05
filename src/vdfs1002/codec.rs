use std::io::Read;

use anyhow::{Context, Result, bail};
use flate2::read::{GzDecoder, ZlibDecoder};

use super::format::{
    COMPRESSED_CRC_OFFSET, COMPRESSED_DESCRIPTOR_SIZE, COMPRESSED_FLAG_STORED, COMPRESSED_LAYOUT,
    COMPRESSED_MAX_TABLE_GAP, COMPRESSED_TABLE_ENTRY, MAX_READ_BYTES, kernel_crc32, le_u16, le_u32,
    le_u64,
};

pub(super) struct CompressedDescriptor {
    pub(super) start: usize,
    pub(super) codec: [u8; 3],
    pub(super) count: u16,
    pub(super) unpacked: u64,
}

pub(super) fn compressed_descriptor(blob: &[u8]) -> Result<Option<CompressedDescriptor>> {
    let Some(start) = blob.len().checked_sub(COMPRESSED_DESCRIPTOR_SIZE) else {
        return Ok(None);
    };
    // `I` marks the same descriptor layout as `C`; the Samsung mkfs.vdfs chunk tables use it.
    if !matches!(blob[start], b'C' | b'I') {
        return Ok(None);
    }
    let codec = [blob[start + 1], blob[start + 2], blob[start + 3]];
    if !matches!(&codec, b"Zip" | b"Gzp" | b"Lzo" | b"Zst") {
        return Ok(None);
    }
    let count = le_u16(blob, start + 4)?;
    let layout = le_u16(blob, start + 6)?;
    let unpacked = le_u64(blob, start + 8)?;
    if layout != COMPRESSED_LAYOUT {
        bail!("unsupported compressed descriptor layout {layout}");
    }
    Ok(Some(CompressedDescriptor {
        start,
        codec,
        count,
        unpacked,
    }))
}

pub(super) fn decompress_blob(blob: &[u8], descriptor: &CompressedDescriptor) -> Result<Vec<u8>> {
    let start = descriptor.start;
    let codec = descriptor.codec;
    let count = descriptor.count as usize;
    let unpacked = descriptor.unpacked;
    if codec == *b"Zst" {
        bail!("compression codec Zst is not supported");
    }
    if count == 0 || unpacked > MAX_READ_BYTES {
        bail!("compressed descriptor has no chunks or an oversized unpacked size");
    }
    let table = locate_chunk_table(blob, start, count)?;

    let gzip = &codec == b"Gzp";
    let mut out: Vec<u8> = Vec::with_capacity(unpacked as usize);
    for index in 0..count {
        let entry = table + COMPRESSED_TABLE_ENTRY * index;
        let tag = &blob[entry..entry + 2];
        let flags = le_u16(blob, entry + 2)?;
        let length = le_u32(blob, entry + 4)? as u64;
        let offset = le_u64(blob, entry + 8)?;
        if tag != b"XT" || flags & !COMPRESSED_FLAG_STORED != 0 || length == 0 {
            bail!("invalid compressed chunk entry {index}");
        }
        let chunk_end = offset
            .checked_add(length)
            .filter(|end| *end <= table as u64)
            .with_context(|| format!("compressed chunk {index} exceeds the chunk area"))?;
        let chunk = &blob[offset as usize..chunk_end as usize];
        let remaining = unpacked - out.len() as u64;
        let part = if flags & COMPRESSED_FLAG_STORED != 0 {
            chunk.to_vec()
        } else if &codec == b"Lzo" {
            crate::lzo1x::decompress(chunk, remaining as usize)
                .with_context(|| format!("compressed chunk {index} is truncated or corrupt"))?
        } else {
            inflate(chunk, gzip, remaining + 1)
                .with_context(|| format!("compressed chunk {index} is truncated or corrupt"))?
        };
        if part.len() as u64 > remaining {
            bail!("decompressed output exceeds the declared size");
        }
        out.extend_from_slice(&part);
    }
    if out.len() as u64 != unpacked {
        bail!("decompressed {} bytes, expected {unpacked}", out.len());
    }
    Ok(out)
}

/// Finds the chunk table by the descriptor CRC, which covers everything from the table start to the
/// descriptor. Some builds leave padding between the last table entry and the descriptor, so the
/// table start is searched backwards from the usual position, and only at `XT` tags.
pub(super) fn locate_chunk_table(blob: &[u8], start: usize, count: usize) -> Result<usize> {
    let stored_crc = le_u32(blob, start + COMPRESSED_CRC_OFFSET)?;
    let mut tail = blob[start..].to_vec();
    tail[COMPRESSED_CRC_OFFSET..COMPRESSED_CRC_OFFSET + 4].fill(0);
    let latest = start
        .checked_sub(COMPRESSED_TABLE_ENTRY * count)
        .context("compressed chunk table exceeds the blob")?;
    let earliest = latest.saturating_sub(COMPRESSED_MAX_TABLE_GAP);
    for table in (earliest..=latest).rev() {
        if &blob[table..table + 2] != b"XT" {
            continue;
        }
        if kernel_crc32(&[&blob[table..start], &tail]) == stored_crc {
            return Ok(table);
        }
    }
    bail!("compressed descriptor CRC mismatch");
}

fn inflate(chunk: &[u8], gzip: bool, limit: u64) -> std::io::Result<Vec<u8>> {
    let decoder: Box<dyn Read> = if gzip {
        Box::new(GzDecoder::new(chunk))
    } else {
        Box::new(ZlibDecoder::new(chunk))
    };
    let mut out = Vec::new();
    decoder.take(limit).read_to_end(&mut out)?;
    Ok(out)
}
