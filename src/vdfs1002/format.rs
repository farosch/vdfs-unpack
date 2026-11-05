use std::collections::HashMap;

use anyhow::{Context, Result, bail};

pub(super) const SECTOR_SIZE: u64 = 512;
pub(super) const SUPERBLOCK_OFFSET: u64 = 2 * SECTOR_SIZE;
pub(super) const EXTENDED_OFFSET: u64 = 3 * SECTOR_SIZE;
pub(super) const EXTENDED_SIZE: u64 = 5 * SECTOR_SIZE;
pub(super) const META_EXTENT_COUNT: usize = 96;
pub(super) const CATALOG_INODE: u64 = 3;
pub(super) const EXTENT_INODE: u64 = 5;
pub(super) const EXTENT_KEY_SIZE: usize = 24;
pub(super) const EXTENT_RECORD_SIZE: usize = 40;
pub(super) const SPECIAL_INODE_COUNT: usize = 5;
pub(super) const NODE_INDEX: u32 = 1;
pub(super) const NODE_LEAF: u32 = 2;
pub(super) const NODE_HEADER_SIZE: usize = 32;
pub(super) const KEY_FIXED_SIZE: usize = 26;
pub(super) const MAX_BASE_TABLE_SIZE: u64 = 1 << 20;
pub(super) const RECORD_FOLDER: u8 = 1;
pub(super) const RECORD_FILE: u8 = 2;
pub(super) const RECORD_HARDLINK: u8 = 3;
pub(super) const RECORD_INDIRECT_LINK: u8 = 5;
pub(super) const FORK_SIZE: usize = 232;
/// Offset of the u16 POSIX mode from the end of a record's key.
pub(super) const RECORD_MODE_OFFSET: usize = 32;
/// POSIX file-type bits for a symbolic link.
pub(super) const S_IFLNK: u16 = 0o120000;
/// POSIX file-type mask in a stored mode.
pub(super) const S_IFMT: u16 = 0o170000;
pub(super) const FORK_EXTENT_COUNT: usize = 13;
pub(super) const MAX_READ_BYTES: u64 = 256 << 20;
pub(super) const COMPRESSED_DESCRIPTOR_SIZE: usize = 32;
pub(super) const COMPRESSED_LAYOUT: u16 = 4;
pub(super) const COMPRESSED_TABLE_ENTRY: usize = 16;
pub(super) const COMPRESSED_CRC_OFFSET: usize = 16;
pub(super) const COMPRESSED_FLAG_STORED: u16 = 1;
pub(super) const COMPRESSED_MAX_TABLE_GAP: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub begin: u64,
    pub length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogRecord {
    pub leaf_node: u32,
    pub parent_id: u64,
    pub object_id: u64,
    pub record_type: u8,
    pub name: Vec<u8>,
    pub mode: Option<u16>,
    pub size: u64,
    pub blocks: u64,
    pub extents: Vec<Extent>,
}

impl CatalogRecord {
    pub fn type_name(&self) -> String {
        match self.record_type {
            RECORD_FOLDER => "folder".to_string(),
            RECORD_FILE => "file".to_string(),
            RECORD_HARDLINK => "hardlink".to_string(),
            RECORD_INDIRECT_LINK => "indirect_link".to_string(),
            other => format!("unknown_{other}"),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "leaf_node": self.leaf_node,
            "parent_id": self.parent_id,
            "object_id": self.object_id,
            "type": self.type_name(),
            "name": String::from_utf8_lossy(&self.name),
        })
    }
}

/// Overflow (iblock, extent) pairs keyed by object id, sorted by iblock.
pub type Overflow = HashMap<u64, Vec<(u64, Extent)>>;

/// Maps inode number to `(last_index, records_offset)` from the base table.
pub(super) type Translation = HashMap<u64, (u64, u64)>;

pub(super) struct Node<'a> {
    pub(super) id: u32,
    pub(super) node_type: u32,
    pub(super) next_node: u32,
    pub(super) offsets: Vec<usize>,
    pub(super) data: &'a [u8],
}

pub(super) fn span(data: &[u8], offset: u64, length: u64) -> Result<&[u8]> {
    let end = offset
        .checked_add(length)
        .filter(|end| *end <= data.len() as u64)
        .with_context(|| format!("read of {length} bytes at {offset} is outside the image"))?;
    Ok(&data[offset as usize..end as usize])
}

fn field<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    let end = offset.checked_add(N).context("offset overflows")?;
    let bytes = data
        .get(offset..end)
        .with_context(|| format!("read of {N} bytes at {offset} is outside the buffer"))?;
    Ok(bytes.try_into().expect("slice length equals N"))
}

pub(super) fn le_u16(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(field(data, offset)?))
}

/// The POSIX mode stored after the key. Absent when the record is too short to hold it,
/// or when the stored value has no file-type bits (real records always set them).
pub(super) fn parse_mode(record: &[u8], key_length: usize) -> Option<u16> {
    let offset = key_length.checked_add(RECORD_MODE_OFFSET)?;
    let bytes = record.get(offset..offset + 2)?;
    let mode = u16::from_le_bytes([bytes[0], bytes[1]]);
    (mode & S_IFMT != 0).then_some(mode)
}

pub(super) fn le_u32(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(field(data, offset)?))
}

pub(super) fn le_u64(data: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(field(data, offset)?))
}

/// zlib-style CRC over the concatenated parts, with the kernel's init and final-XOR convention.
pub(super) fn kernel_crc32(parts: &[&[u8]]) -> u32 {
    let mut hasher = crc32fast::Hasher::new_with_initial(0xFFFF_FFFF);
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize() ^ 0xFFFF_FFFF
}

pub(super) fn record<'a>(node: &Node<'a>, index: usize, key_minimum: usize) -> Result<&'a [u8]> {
    let data: &'a [u8] = node.data;
    let offset = node.offsets[index];
    let key_length = le_u16(data, offset + 4)? as usize;
    let record_length = le_u16(data, offset + 6)? as usize;
    if key_length < key_minimum || record_length < key_length {
        bail!("node {} record {index} has an invalid length", node.id);
    }
    if offset + record_length > data.len() {
        bail!("node {} record {index} runs past the node", node.id);
    }
    Ok(&data[offset..offset + record_length])
}

pub(super) fn parse_fork(
    node_id: u32,
    index: usize,
    bytes: &[u8],
    key_length: usize,
) -> Result<(u64, u64, Vec<Extent>)> {
    let start = bytes
        .len()
        .checked_sub(FORK_SIZE)
        .filter(|start| *start >= key_length)
        .with_context(|| format!("node {node_id} record {index} is too short for a file fork"))?;
    let size = le_u64(bytes, start)?;
    let blocks = le_u64(bytes, start + 8)?;
    let mut extents = Vec::new();
    for position in 0..FORK_EXTENT_COUNT {
        let at = start + 16 + 16 * position;
        let extent = Extent {
            begin: le_u64(bytes, at)?,
            length: le_u64(bytes, at + 8)?,
        };
        if extent.length != 0 {
            extents.push(extent);
        }
    }
    Ok((size, blocks, extents))
}

/// Inline extents followed by overflow extents. Overflow must continue the block count without gaps.
pub(super) fn file_extents(record: &CatalogRecord, overflow: &Overflow) -> Result<Vec<Extent>> {
    if record.record_type != RECORD_FILE {
        bail!("object {} is not a file", record.object_id);
    }
    let mut expected = record
        .extents
        .iter()
        .fold(0u64, |sum, extent| sum.saturating_add(extent.length));
    let extra: &[(u64, Extent)] = match overflow.get(&record.object_id) {
        Some(items) => items,
        None => &[],
    };
    for (iblock, extent) in extra {
        if *iblock != expected {
            bail!(
                "object {} overflow extent at iblock {iblock} does not follow block {expected}",
                record.object_id
            );
        }
        expected = expected.saturating_add(extent.length);
    }
    if expected != record.blocks {
        bail!(
            "object {} extents cover {expected} blocks, fork declares {}",
            record.object_id,
            record.blocks
        );
    }
    Ok(record
        .extents
        .iter()
        .copied()
        .chain(extra.iter().map(|(_, extent)| *extent))
        .collect())
}
