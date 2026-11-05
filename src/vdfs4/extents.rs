use anyhow::{Result, bail};
use std::collections::HashMap;

use super::btree::Btree;
use super::disk::*;
use super::reader::ImageReader;

/// Build a map from object_id -> ordered list of (iblock, extent) for every
/// extent stored in the extents overflow tree. Files whose fork has more than
/// VDFS4_EXTENTS_COUNT_IN_FORK extents keep the remainder here.
pub fn build_extent_overflow_map(
    _reader: &ImageReader,
    ext_tree: &Btree,
) -> Result<HashMap<u64, Vec<(u64, Vdfs4Extent)>>> {
    let mut map: HashMap<u64, Vec<(u64, Vdfs4Extent)>> = HashMap::new();
    let recs = ext_tree.collect_all_records()?;
    for rec in recs {
        if rec.len() < 40 {
            continue;
        }
        // vdfs4_exttree_lrecord: gen_key(8) + object_id(8) + iblock(8) + lextent(16)
        let oid = read_u64_le(&rec, 8);
        let iblock = read_u64_le(&rec, 16);
        let begin = read_u64_le(&rec, 24);
        let length = read_u64_le(&rec, 32);
        if length == 0 {
            continue;
        }
        map.entry(oid)
            .or_default()
            .push((iblock, Vdfs4Extent { begin, length }));
    }
    for v in map.values_mut() {
        v.sort_by_key(|(ib, _)| *ib);
    }
    Ok(map)
}

pub fn extents_resolve_fork(
    fork: &Vdfs4Fork,
    file_size: u64,
    overflow: &HashMap<u64, Vec<(u64, Vdfs4Extent)>>,
    object_id: u64,
) -> Result<Vec<Vdfs4Extent>> {
    if file_size == 0 {
        return Ok(Vec::new());
    }

    let total_blocks = fork.total_blocks_count;
    if total_blocks == 0 {
        return Ok(Vec::new());
    }

    let iexts = fork.extents();
    let mut inline: Vec<(u64, Vdfs4Extent)> = Vec::new();
    let mut inline_end = 0u64;
    for e in iexts.iter().take(VDFS4_EXTENTS_COUNT_IN_FORK) {
        if e.extent.length == 0 {
            continue;
        }
        inline.push((e.iblock, e.extent));
        let end = e.iblock + e.extent.length;
        if end > inline_end {
            inline_end = end;
        }
    }

    let inline_blocks: u64 = inline.iter().map(|(_, e)| e.length).sum();
    let mut result: Vec<Vdfs4Extent> = inline.iter().map(|(_, e)| *e).collect();

    // If the inline extents do not cover the whole file, pull the remaining
    // extents from the overflow tree.
    if (total_blocks > inline_blocks || total_blocks as usize > inline.len())
        && let Some(ov) = overflow.get(&object_id)
    {
        for (ib, ext) in ov {
            if *ib >= inline_end {
                result.push(*ext);
            }
        }
    }

    if result.is_empty() {
        // Fall back to whatever inline extents we have.
        for e in iexts.iter().take(VDFS4_EXTENTS_COUNT_IN_FORK) {
            if e.extent.length != 0 {
                result.push(e.extent);
            }
        }
    }

    Ok(result)
}

pub fn extents_read_data(
    reader: &ImageReader,
    extents: &[Vdfs4Extent],
    mut offset: u64,
    mut length: u64,
) -> Result<Vec<u8>> {
    let mut result = Vec::new();

    for ext in extents {
        if ext.is_empty() || length == 0 {
            continue;
        }

        let ext_start = ext.begin;
        let ext_len = ext.length;
        let ext_bytes = ext_len.saturating_mul(reader.sbi.block_size as u64);

        if offset >= ext_bytes {
            offset -= ext_bytes;
            continue;
        }

        let read_from = ext_start
            .saturating_mul(reader.sbi.block_size as u64)
            .saturating_add(offset);
        let remaining = ext_bytes - offset;
        let to_read = length.min(remaining);

        let block_start = read_from / reader.sbi.block_size as u64;
        let block_count = to_read.div_ceil(reader.sbi.block_size as u64);
        let data = reader.read_extent_data(&Vdfs4Extent {
            begin: block_start,
            length: block_count,
        })?;

        let start_in_data = (read_from % reader.sbi.block_size as u64) as usize;
        let end_in_data = start_in_data + to_read as usize;
        if end_in_data <= data.len() {
            result.extend_from_slice(&data[start_in_data..end_in_data]);
        }

        length = length.saturating_sub(to_read);
        offset = 0;
    }

    if length > 0 {
        bail!("Short read: expected {} more bytes", length);
    }

    Ok(result)
}

pub fn open_ext_tree<'a>(reader: &'a ImageReader<'a>) -> Result<Btree<'a>> {
    Btree::open(reader, VDFS4_EXTENTS_TREE_INO)
}
