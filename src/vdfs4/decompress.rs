use anyhow::{Context, Result, anyhow, bail};
use ctr::cipher::{KeyIvInit, StreamCipher};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use super::disk::*;
use super::extents::{extents_read_data, extents_resolve_fork};
use super::reader::ImageReader;

pub type AesKey = [u8; 16];
type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

#[derive(Clone, Copy, Debug)]
enum ComprType {
    Zlib,
    Gzip,
    Lzo,
    Zstd,
}

/// Returns true if the catalog record flags indicate a compressed file.
/// VDFS4 uses the VDFS4_COMPRESSED_FILE (bit 13) inode flag for extent-based
/// compressed files (descriptor at the tail of the fork data).
pub fn is_compressed_file(flags: u32) -> bool {
    flags & VDFS4_COMPRESSED_FILE != 0
}

/// Map a magic to a compression type. For inline compressed data the magic is
/// the bare 3-char name ("Zip", ...); for the on-disk descriptor it is stored
/// as <prefix-byte>"Zip" (e.g. "hZip") with the name at bytes [1..4].
fn detect_compr_type(magic: &[u8]) -> Option<ComprType> {
    let name: &[u8] = if magic.len() >= 4 {
        &magic[1..4]
    } else if magic.len() >= 3 {
        &magic[..3]
    } else {
        return None;
    };
    match name {
        // Zhw is zlib with chunks padded to 16 bytes. The zlib decoder stops at
        // the end of its stream, so the padding is never read.
        b"Zip" | b"Zhw" => Some(ComprType::Zlib),
        b"Gzp" => Some(ComprType::Gzip),
        b"Lzo" => Some(ComprType::Lzo),
        b"Zst" => Some(ComprType::Zstd),
        _ => None,
    }
}

/// `max_out` bounds the output of codecs that cannot find their own end.
fn decompress_with(data: &[u8], ctype: ComprType, max_out: usize) -> Result<Vec<u8>> {
    match ctype {
        ComprType::Zlib => {
            let mut decoder = flate2::read::ZlibDecoder::new(data);
            let mut output = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut output)
                .context("zlib decompression failed")?;
            Ok(output)
        }
        ComprType::Gzip => {
            let mut decoder = flate2::read::GzDecoder::new(data);
            let mut output = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut output)
                .context("gzip decompression failed")?;
            Ok(output)
        }
        ComprType::Zstd => {
            let mut decoder =
                zstd::stream::read::Decoder::new(data).context("zstd decoder init failed")?;
            let mut output = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut output)
                .context("zstd decompression failed")?;
            Ok(output)
        }
        ComprType::Lzo => crate::lzo1x::decompress(data, max_out),
    }
}

/// True if the 4 descriptor-magic bytes form a valid compressed descriptor:
/// byte[0] is an arbitrary prefix, bytes[1..4] are the compression name.
fn valid_comp_descr_magic(m: &[u8; 4]) -> bool {
    matches!(&m[1..4], b"Zip" | b"Zhw" | b"Gzp" | b"Lzo" | b"Zst")
}

/// Digest used for the per-chunk and descriptor hashes of an authenticated
/// compressed file. The first descriptor magic byte selects it: 'C' means
/// unhashed, 'I' MD5, 'H' SHA-1, 'h' SHA-256.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DigestAlgo {
    Md5,
    Sha1,
    Sha256,
}

impl DigestAlgo {
    fn from_magic(prefix: u8) -> Result<Option<Self>> {
        match prefix {
            b'C' => Ok(None),
            b'I' => Ok(Some(Self::Md5)),
            b'H' => Ok(Some(Self::Sha1)),
            b'h' => Ok(Some(Self::Sha256)),
            _ => bail!("Unsupported descriptor authentication type"),
        }
    }

    fn len(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }

    fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Md5 => md5::Md5::digest(data).to_vec(),
            Self::Sha1 => sha1::Sha1::digest(data).to_vec(),
            Self::Sha256 => Sha256::digest(data).to_vec(),
        }
    }
}

/// Locate the compressed file descriptor within the compressed blob.
/// The descriptor sits at the very tail (right before any optional auth
/// trailer), and is identified by its 3-char compression name plus a sane
/// layout version. Returns the byte offset of the descriptor start.
fn find_descriptor(data: &[u8]) -> Option<usize> {
    let name_ok = |c: usize| -> bool {
        if c + 12 > data.len() {
            return false;
        }
        matches!(
            &data[c + 9..c + 12],
            b"Zip" | b"Zhw" | b"Gzp" | b"Lzo" | b"Zst"
        )
    };
    let layout_ok = |c: usize| -> bool {
        if c + 16 > data.len() {
            return false;
        }
        let v = u16::from_le_bytes([data[c + 14], data[c + 15]]);
        v == VDFS4_COMPR_LAYOUT_VER_05 || v == VDFS4_COMPR_LAYOUT_VER_06
    };

    for &size in &[VDFS4_COMPR_FILE_DESC_LEN_06, VDFS4_COMPR_FILE_DESC_LEN_05] {
        if data.len() >= size {
            let c = data.len() - size;
            if name_ok(c) && layout_ok(c) {
                return Some(c);
            }
        }
    }
    None
}

pub fn decompress_file(
    reader: &ImageReader,
    fork: &Vdfs4Fork,
    object_id: u64,
    overflow: &HashMap<u64, Vec<(u64, Vdfs4Extent)>>,
) -> Result<Vec<u8>> {
    if fork.total_blocks_count == 0 {
        // Inline compressed data: vdfs4_comp_inline_data layout is
        // comp_type[3], comp_size (1 byte), data[]. fork.size_in_bytes holds
        // the *uncompressed* size (the file's i_size).
        let ctype =
            detect_compr_type(&fork.data[0..3]).context("unknown inline compression type")?;
        let comp_size = fork.data[3] as usize;
        let end = 4 + comp_size;
        if end > fork.data.len() {
            bail!("Inline compressed data exceeds fork");
        }
        let compressed = &fork.data[4..end];
        let result = decompress_with(compressed, ctype, fork.size_in_bytes as usize)?;
        let file_size = fork.size_in_bytes as usize;
        if result.len() != file_size {
            bail!("Inline decompressed size mismatch");
        }
        return Ok(result);
    }

    // Extent-based compressed file. fork.size_in_bytes holds the COMPRESSED
    // size (comp_size); the fork's extents point at that blob in the image.
    // The blob layout is: [compressed chunks][comp_extent table][descriptor].
    // Each vdfs4_comp_extent.start is a byte offset WITHIN this blob.
    let comp_total = fork.size_in_bytes;
    let extents = extents_resolve_fork(fork, comp_total, overflow, object_id)?;
    if extents.is_empty() {
        bail!("Compressed file has no extents");
    }
    let data = extents_read_data(reader, &extents, 0, comp_total)?;

    decompress_blob_keyed(&data, reader.aes_key.as_ref())
}

// Layout-05 compressed descriptor (32 bytes; VDFS2006 images such as JazzM).
// Tail layout: magic[4] ("C" or "h" plus codec), extents u16 @4, layout u16 @6 (5),
// unpacked size u64 @8, CRC u32 @16 (not checked), log chunk u32 @20, zero pad @24.
// Before it: one 16-byte "XT" entry per chunk, then for "h" blobs one SHA-256 per
// chunk, one SHA-256 over the descriptor, and a signature block of 0, 128 or 256 bytes.
// Chunk hashes are verified. The descriptor hash and signature are not: no tested
// hash input matches the descriptor digest and the signature has no vendor key.

fn is_layout5_blob(data: &[u8]) -> bool {
    if data.len() < VDFS4_COMPR_FILE_DESC_LEN_05 {
        return false;
    }
    let descriptor = data.len() - VDFS4_COMPR_FILE_DESC_LEN_05;
    let magic = &data[descriptor..descriptor + 4];
    (magic[0] == b'C' || magic[0] == b'h')
        && detect_compr_type(magic).is_some()
        && u16::from_le_bytes([data[descriptor + 6], data[descriptor + 7]])
            == VDFS4_COMPR_LAYOUT_VER_05
}

struct Layout5Chunk {
    flags: u16,
    start: usize,
    len: usize,
}

fn layout5_chunk(data: &[u8], table: usize, index: usize) -> Result<Layout5Chunk> {
    let at = table + 16 * index;
    if data.get(at..at + 2) != Some(b"XT") {
        bail!("Invalid compressed extent magic");
    }
    let flags = u16::from_le_bytes([data[at + 2], data[at + 3]]);
    let len = usize::try_from(u32::from_le_bytes(data[at + 4..at + 8].try_into()?))?;
    let start = usize::try_from(u64::from_le_bytes(data[at + 8..at + 16].try_into()?))?;
    if len == 0 || flags & !VDFS4_CHUNK_FLAG_UNCOMPR != 0 {
        bail!("Invalid compressed chunk length/flags");
    }
    if start.checked_add(len).context("Chunk range overflow")? > table {
        bail!("Compressed chunk exceeds table");
    }
    Ok(Layout5Chunk { flags, start, len })
}

fn decompress_layout5(data: &[u8]) -> Result<Vec<u8>> {
    let descriptor = data.len() - VDFS4_COMPR_FILE_DESC_LEN_05;
    let magic = &data[descriptor..descriptor + 4];
    let ctype = detect_compr_type(magic).context("unknown compression type in descriptor")?;
    let count = u16::from_le_bytes([data[descriptor + 4], data[descriptor + 5]]) as usize;
    if count == 0 {
        bail!("Compressed file has no chunks");
    }
    let expected_size = usize::try_from(u64::from_le_bytes(
        data[descriptor + 8..descriptor + 16].try_into()?,
    ))?;
    if expected_size > 512 * 1024 * 1024 {
        bail!("Decompressed file exceeds local 512 MiB limit");
    }
    let hashed = magic[0] == b'h';
    let signatures: &[usize] = if hashed { &[0, 128, 256] } else { &[0] };
    let table = signatures
        .iter()
        .find_map(|&signature| {
            let overhead = 16 * count
                + if hashed {
                    32 * (count + 1) + signature
                } else {
                    0
                };
            let table = descriptor.checked_sub(overhead)?;
            let verified = (0..count).all(|index| {
                let Ok(chunk) = layout5_chunk(data, table, index) else {
                    return false;
                };
                if !hashed {
                    return true;
                }
                let digest = &data[table + 16 * count + 32 * index..][..32];
                Sha256::digest(&data[chunk.start..chunk.start + chunk.len]).as_slice() == digest
            });
            verified.then_some(table)
        })
        .context("No signature length gives verified compressed chunk hashes")?;

    let mut result = Vec::with_capacity(expected_size);
    for index in 0..count {
        let chunk = layout5_chunk(data, table, index)?;
        let bytes = &data[chunk.start..chunk.start + chunk.len];
        if chunk.flags & VDFS4_CHUNK_FLAG_UNCOMPR != 0 {
            result.extend_from_slice(bytes);
        } else {
            let decompressed = decompress_with(bytes, ctype, expected_size - result.len())
                .with_context(|| format!("decompressing chunk #{} (len={})", index, chunk.len))?;
            result.extend_from_slice(&decompressed);
        }
        if result.len() > expected_size {
            bail!("Decompressed output exceeds declared size");
        }
    }
    if result.len() != expected_size {
        bail!(
            "Decompressed size mismatch: got {}, expected {}",
            result.len(),
            expected_size
        );
    }
    Ok(result)
}

#[cfg(test)]
mod layout5_tests {
    use super::*;
    use std::io::Write;

    fn zlib(input: &[u8]) -> Vec<u8> {
        let mut writer =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        writer.write_all(input).unwrap();
        writer.finish().unwrap()
    }

    fn blob(hashed: bool) -> (Vec<u8>, Vec<u8>) {
        let first = b"stored chunk".to_vec();
        let second = b"zlib chunk, physically after the stored one".to_vec();
        let compressed = zlib(&second);
        let mut data = first.clone();
        let second_start = data.len();
        data.extend_from_slice(&compressed);
        let chunks = [
            (0usize, first.len(), 1u16),
            (second_start, compressed.len(), 0u16),
        ];
        for &(start, len, flags) in &chunks {
            data.extend_from_slice(b"XT");
            data.extend_from_slice(&flags.to_le_bytes());
            data.extend_from_slice(&(len as u32).to_le_bytes());
            data.extend_from_slice(&(start as u64).to_le_bytes());
        }
        if hashed {
            data.extend_from_slice(&Sha256::digest(&first));
            data.extend_from_slice(&Sha256::digest(&compressed));
            data.extend_from_slice(&[0u8; 32]);
            data.extend_from_slice(&[0u8; 128]);
        }
        let mut descriptor = [0u8; 32];
        descriptor[0] = if hashed { b'h' } else { b'C' };
        descriptor[1..4].copy_from_slice(b"Zip");
        descriptor[4..6].copy_from_slice(&2u16.to_le_bytes());
        descriptor[6..8].copy_from_slice(&VDFS4_COMPR_LAYOUT_VER_05.to_le_bytes());
        descriptor[8..16].copy_from_slice(&((first.len() + second.len()) as u64).to_le_bytes());
        data.extend_from_slice(&descriptor);
        let expected = [first.as_slice(), second.as_slice()].concat();
        (data, expected)
    }

    #[test]
    fn decodes_hashed_layout5_with_stored_and_compressed_chunks() {
        let (data, expected) = blob(true);
        assert!(is_layout5_blob(&data));
        assert_eq!(decompress_blob(&data).unwrap(), expected);
    }

    #[test]
    fn decodes_unhashed_layout5() {
        let (data, expected) = blob(false);
        assert_eq!(decompress_blob(&data).unwrap(), expected);
    }

    #[test]
    fn rejects_corrupt_chunk_hash() {
        let (mut data, _) = blob(true);
        data[0] ^= 1;
        assert!(decompress_blob(&data).is_err());
    }

    /// One unhashed compressed chunk with the given codec name in its descriptor.
    fn single_chunk_blob(name: &[u8; 3], stream: &[u8], declared_len: u64) -> Vec<u8> {
        let mut data = stream.to_vec();
        data.extend_from_slice(b"XT");
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&(stream.len() as u32).to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes());
        let mut descriptor = [0u8; 32];
        descriptor[0] = b'C';
        descriptor[1..4].copy_from_slice(name);
        descriptor[4..6].copy_from_slice(&1u16.to_le_bytes());
        descriptor[6..8].copy_from_slice(&VDFS4_COMPR_LAYOUT_VER_05.to_le_bytes());
        descriptor[8..16].copy_from_slice(&declared_len.to_le_bytes());
        data.extend_from_slice(&descriptor);
        data
    }

    // liblzo2 output for b"VDFS block " * 50.
    const LZO_TEXT: &str =
        "085644465320626c6f636b202000ec28000c6f636b205644465320626c6f636b20110000";

    use crate::hex::decode as unhex;

    #[test]
    fn decodes_lzo_layout5_chunk() {
        let payload = b"VDFS block ".repeat(50);
        let data = single_chunk_blob(b"Lzo", &unhex(LZO_TEXT), payload.len() as u64);
        assert_eq!(decompress_blob(&data).unwrap(), payload);
    }

    #[test]
    fn decodes_zhw_chunk_with_16_byte_padding() {
        let payload = b"VDFS hw page ".repeat(40);
        let mut stream = zlib(&payload);
        while !stream.len().is_multiple_of(16) {
            stream.push(0);
        }
        let data = single_chunk_blob(b"Zhw", &stream, payload.len() as u64);
        assert_eq!(decompress_blob(&data).unwrap(), payload);
    }

    #[test]
    fn rejects_lzo_chunk_exceeding_declared_size() {
        let data = single_chunk_blob(b"Lzo", &unhex(LZO_TEXT), 100);
        assert!(decompress_blob(&data).is_err());
    }

    #[test]
    fn rejects_truncated_lzo_chunk() {
        let stream = unhex(LZO_TEXT);
        let data = single_chunk_blob(b"Lzo", &stream[..stream.len() - 1], 550);
        assert!(decompress_blob(&data).is_err());
    }

    #[test]
    fn rejects_declared_size_mismatch() {
        let (mut data, _) = blob(true);
        let descriptor = data.len() - 32;
        data[descriptor + 8] ^= 1;
        assert!(decompress_blob(&data).is_err());
    }
}

/// Decrypts one stored chunk. mkfs uses AES-128-CTR with IV = nonce[8] || LE64(block index),
/// where block index = chunk_index * 2^log_chunk_size / 16 and each chunk is its own stream.
fn decrypt_chunk(
    key: &AesKey,
    nonce: &[u8; VDFS4_AES_NONCE_SIZE],
    chunk_index: u64,
    log_chunk_size: u32,
    stored: &[u8],
) -> Result<Vec<u8>> {
    if log_chunk_size >= 64 {
        bail!("Invalid chunk size exponent: {}", log_chunk_size);
    }
    let block_index = chunk_index
        .checked_mul(1u64 << log_chunk_size)
        .context("Chunk counter overflow")?
        / 16;
    let mut iv = [0u8; 16];
    iv[..VDFS4_AES_NONCE_SIZE].copy_from_slice(nonce);
    iv[VDFS4_AES_NONCE_SIZE..].copy_from_slice(&block_index.to_le_bytes());
    let mut buf = stored.to_vec();
    let mut cipher = Aes128Ctr::new_from_slices(key, &iv)
        .map_err(|e| anyhow!("Invalid AES key or IV: {}", e))?;
    cipher.apply_keystream(&mut buf);
    Ok(buf)
}

#[cfg(test)]
fn decompress_blob(data: &[u8]) -> Result<Vec<u8>> {
    decompress_blob_keyed(data, None)
}

fn decompress_blob_keyed(data: &[u8], key: Option<&AesKey>) -> Result<Vec<u8>> {
    if is_layout5_blob(data) {
        return decompress_layout5(data);
    }
    let descr_offset =
        find_descriptor(data).context("Could not locate compressed file descriptor")?;
    if data.len() - descr_offset != std::mem::size_of::<Vdfs4CompFileDescr>() {
        bail!("Only complete layout-06 descriptors are supported");
    }
    let descr: Vdfs4CompFileDescr = unsafe {
        std::ptr::read_unaligned(data[descr_offset..].as_ptr() as *const Vdfs4CompFileDescr)
    };

    if !valid_comp_descr_magic(&descr.magic) {
        bail!(
            "Invalid compressed file descriptor magic: {:?}",
            descr.magic
        );
    }

    let ctype =
        detect_compr_type(&descr.magic).context("unknown compression type in descriptor")?;

    let comp_extents_count = descr.extents_num as usize;
    let extent_struct_size = std::mem::size_of::<Vdfs4CompExtent>();
    let algo = DigestAlgo::from_magic(descr.magic[0])?;
    let digest_len = algo.map_or(0, DigestAlgo::len);
    let trailer = if algo.is_some() {
        let signature_bytes = match descr.sign_type {
            0 => 0,
            1 => 128,
            2 => 256,
            _ => bail!("Unknown hashed-file signature type"),
        };
        digest_len * (comp_extents_count + 1) + signature_bytes
    } else {
        0
    };
    let table_offset = descr_offset
        .checked_sub(trailer + comp_extents_count * extent_struct_size)
        .context("Compressed table exceeds blob")?;
    let hash_offset = table_offset + comp_extents_count * extent_struct_size;
    if let Some(algo) = algo {
        let mut descriptor_bytes = data[descr_offset..].to_vec();
        descriptor_bytes[24..28].fill(0);
        let hash = algo.digest(&descriptor_bytes);
        let start = hash_offset + digest_len * comp_extents_count;
        if hash.as_slice() != &data[start..start + digest_len] {
            bail!("Compressed descriptor digest mismatch");
        }
    }
    let expected_size = usize::try_from(descr.unpacked_size)?;
    if expected_size > 512 * 1024 * 1024 {
        bail!("Decompressed file exceeds local 512 MiB limit");
    }
    let mut result = Vec::new();
    let mut off = table_offset;
    let mut read = 0usize;
    while read < comp_extents_count {
        let comp_ext: Vdfs4CompExtent =
            unsafe { std::ptr::read_unaligned(data[off..].as_ptr() as *const Vdfs4CompExtent) };
        off += extent_struct_size;
        read += 1;

        let chunk_len = comp_ext.len_bytes as usize;
        if chunk_len == 0 || data[off - extent_struct_size..off - extent_struct_size + 2] != *b"XT"
        {
            bail!("Invalid compressed extent magic/length");
        }
        let start = comp_ext.start as usize;
        let chunk_end = start
            .checked_add(chunk_len)
            .context("Chunk range overflow")?;
        if chunk_end > table_offset
            || comp_ext.flags & !(VDFS4_CHUNK_FLAG_UNCOMPR | VDFS4_CHUNK_FLAG_ENCRYPTED) != 0
        {
            bail!("Invalid compressed chunk bounds/flags");
        }
        let stored = &data[start..chunk_end];
        if let Some(algo) = algo {
            let hash = algo.digest(stored);
            let start = hash_offset + digest_len * (read - 1);
            if hash.as_slice() != &data[start..start + digest_len] {
                bail!("Compressed chunk digest mismatch");
            }
        }

        let decrypted = if comp_ext.flags & VDFS4_CHUNK_FLAG_ENCRYPTED != 0 {
            let key =
                key.context("Image has encrypted chunks but no AES key was given (use --aes-key)")?;
            let nonce = descr.aes_nonce;
            let log_chunk_size = descr.log_chunk_size;
            Some(decrypt_chunk(
                key,
                &nonce,
                (read - 1) as u64,
                log_chunk_size,
                stored,
            )?)
        } else {
            None
        };
        let chunk = decrypted.as_deref().unwrap_or(stored);

        if comp_ext.flags & VDFS4_CHUNK_FLAG_UNCOMPR != 0 {
            result.extend_from_slice(chunk);
        } else {
            let decompressed = decompress_with(chunk, ctype, expected_size - result.len())
                .with_context(|| {
                    format!(
                        "decompressing chunk #{} (start={} len={})",
                        read, start, chunk_len
                    )
                })?;
            result.extend_from_slice(&decompressed);
        }
        if result.len() > expected_size {
            bail!("Decompressed output exceeds declared size");
        }
    }
    if result.len() != expected_size {
        bail!(
            "Decompressed size mismatch: got {}, expected {}",
            result.len(),
            expected_size
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex::encode as hex;
    use std::io::Write;

    #[test]
    fn decrypt_chunk_matches_aes_ctr_reference() {
        // Reference: openssl enc -aes-128-ctr with IV = nonce || LE64(2)
        // over 32 zero bytes. Chunk index 1 with 32-byte chunks gives block 2.
        let key: AesKey = *b"0123456789abcdef";
        let nonce = [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17];
        let expected: [u8; 32] = [
            0x41, 0xb0, 0x41, 0x58, 0xa7, 0x1e, 0x48, 0xa0, 0x1b, 0xb0, 0x66, 0x6f, 0x38, 0x3c,
            0xd1, 0x1c, 0xd0, 0xd0, 0x10, 0x82, 0x3b, 0x68, 0x7f, 0xec, 0x96, 0xa5, 0x67, 0xf5,
            0x95, 0x78, 0x2c, 0xef,
        ];
        let got = decrypt_chunk(&key, &nonce, 1, 5, &[0u8; 32]).unwrap();
        assert_eq!(got, expected);
    }

    fn fixture() -> (Vec<u8>, Vec<u8>) {
        hashed_fixture(b'h')
    }

    fn hashed_fixture(prefix: u8) -> (Vec<u8>, Vec<u8>) {
        let algo = DigestAlgo::from_magic(prefix).unwrap().unwrap();
        let first = b"first logical chunk, physically second";
        let second = b"second logical chunk, physically first";
        let compress = |input: &[u8]| {
            let mut writer =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            writer.write_all(input).unwrap();
            writer.finish().unwrap()
        };
        let a = compress(first);
        let b = compress(second);
        let mut blob = b.clone();
        blob.extend_from_slice(&a);
        for (start, bytes) in [(b.len(), &a), (0, &b)] {
            blob.extend_from_slice(b"XT");
            blob.extend_from_slice(&0u16.to_le_bytes());
            blob.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            blob.extend_from_slice(&(start as u64).to_le_bytes());
        }
        blob.extend_from_slice(&algo.digest(&a));
        blob.extend_from_slice(&algo.digest(&b));
        let mut descriptor = vec![0; 40];
        descriptor[7] = 1;
        descriptor[8..12].copy_from_slice(&[prefix, b'Z', b'i', b'p']);
        descriptor[12..14].copy_from_slice(&2u16.to_le_bytes());
        descriptor[14..16].copy_from_slice(&6u16.to_le_bytes());
        descriptor[16..24].copy_from_slice(&((first.len() + second.len()) as u64).to_le_bytes());
        blob.extend_from_slice(&algo.digest(&descriptor));
        blob.extend_from_slice(&[0; 128]);
        blob.extend_from_slice(&descriptor);
        (blob, [first.as_slice(), second.as_slice()].concat())
    }

    #[test]
    fn hashed_chunks_follow_table_not_physical_order() {
        let (blob, expected) = fixture();
        assert_eq!(decompress_blob(&blob).unwrap(), expected);
    }

    #[test]
    fn rejects_corrupt_chunk_hash() {
        let (mut blob, _) = fixture();
        blob[0] ^= 1;
        assert!(decompress_blob(&blob).is_err());
    }

    #[test]
    fn rejects_corrupt_descriptor_hash() {
        let (mut blob, _) = fixture();
        let position = blob.len() - 24;
        blob[position] ^= 1;
        assert!(decompress_blob(&blob).is_err());
    }

    #[test]
    fn md5_and_sha1_hashed_chunks_verify() {
        for prefix in *b"IH" {
            let (blob, expected) = hashed_fixture(prefix);
            assert_eq!(decompress_blob(&blob).unwrap(), expected);
        }
    }

    #[test]
    fn md5_and_sha1_reject_corrupt_hashes() {
        for prefix in *b"IH" {
            let (mut blob, _) = hashed_fixture(prefix);
            blob[0] ^= 1;
            assert!(decompress_blob(&blob).is_err());

            let (mut blob, _) = hashed_fixture(prefix);
            let position = blob.len() - 24;
            blob[position] ^= 1;
            assert!(decompress_blob(&blob).is_err());
        }
    }

    #[test]
    fn rejects_unknown_authentication_prefix() {
        let (mut blob, _) = fixture();
        let position = blob.len() - 32;
        blob[position] = b'X';
        assert!(decompress_blob(&blob).is_err());
    }

    #[test]
    fn digest_known_answers() {
        assert_eq!(
            hex(&DigestAlgo::Md5.digest(b"abc")),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            hex(&DigestAlgo::Sha1.digest(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&DigestAlgo::Sha256.digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rejects_bad_extent_magic_or_bounds() {
        let (blob, _) = fixture();
        let table = blob.len() - 40 - 128 - 3 * 32 - 2 * 16;
        let mut bad_magic = blob.clone();
        bad_magic[table] ^= 1;
        assert!(decompress_blob(&bad_magic).is_err());
        let mut bad_start = blob;
        bad_start[table + 8..table + 16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decompress_blob(&bad_start).is_err());
    }

    #[test]
    fn rejects_declared_size_mismatch_even_with_valid_hash() {
        let (mut blob, _) = fixture();
        let descriptor = blob.len() - 40;
        blob[descriptor + 16] ^= 1;
        let hash = Sha256::digest(&blob[descriptor..]);
        blob[descriptor - 160..descriptor - 128].copy_from_slice(&hash);
        assert!(decompress_blob(&blob).is_err());
    }
}
