use anyhow::{Context, Result, bail};

use super::disk::*;
use super::superblock::SuperblockInfo;

pub struct SnapshotInfo {
    pub tables: Vec<Vec<Vdfs4BaseTableRecord>>,
}

pub fn build_snapshot_manager(sbi: &SuperblockInfo, image: &[u8]) -> Result<SnapshotInfo> {
    let exsb = &sbi.layout.exsb;
    let tables_extent = exsb.tables;

    if tables_extent.is_empty() {
        bail!("Tables extent is empty");
    }

    let tables_byte_offset = tables_extent.begin as usize * sbi.block_size;
    let base_table: Vdfs4BaseTable =
        unsafe { read_packed(image, tables_byte_offset).context("Failed to read base table")? };

    let mut tables = Vec::new();

    for i in 0..VDFS4_SF_NR {
        let table_offset = base_table.translation_table_offsets[i];
        let last_idx = base_table.last_page_index[i];

        let mut records = Vec::new();
        if last_idx > 0 {
            let record_base = tables_byte_offset + table_offset as usize;
            for idx in 0..=last_idx as usize {
                let rec_offset = record_base + idx * std::mem::size_of::<Vdfs4BaseTableRecord>();
                if rec_offset + std::mem::size_of::<Vdfs4BaseTableRecord>() > image.len() {
                    break;
                }
                let rec: Vdfs4BaseTableRecord = unsafe {
                    read_packed(image, rec_offset).context("Failed to read base table record")?
                };
                records.push(rec);
            }
        }
        tables.push(records);
    }

    Ok(SnapshotInfo { tables })
}

pub fn get_meta_iblock(
    snapshot: &SnapshotInfo,
    sbi: &SuperblockInfo,
    ino: u64,
    page_index: usize,
) -> Result<u64> {
    let sf_idx = sf_index(ino);
    if sf_idx >= VDFS4_SF_NR {
        bail!("Invalid special file inode: {}", ino);
    }

    let is_tree = (VDFS4_CAT_TREE_INO..=VDFS4_XATTR_TREE_INO).contains(&ino);
    let table_page_index = if is_tree {
        let shift = sbi.log_blocks_in_leb + sbi.block_size_shift - 12;
        page_index >> shift
    } else {
        page_index
    };

    let records = &snapshot.tables[sf_idx];
    if table_page_index >= records.len() {
        bail!(
            "page_index {} (table_page_index {}) out of range for ino {} (max {})",
            page_index,
            table_page_index,
            ino,
            records.len() - 1
        );
    }

    Ok(records[table_page_index].meta_iblock)
}

pub fn meta_iblock_to_block(sbi: &SuperblockInfo, meta_iblock: u64) -> Result<u64> {
    let exsb = &sbi.layout.exsb;

    let mut cumulative = 0u64;
    for i in 0..VDFS4_META_BTREE_EXTENTS {
        let extent = exsb.meta[i];
        if extent.is_empty() {
            continue;
        }
        let end = cumulative + extent.length;
        if meta_iblock < end {
            let offset = meta_iblock - cumulative;
            return Ok(extent.begin + offset);
        }
        cumulative = end;
    }

    bail!("meta_iblock {} not found in meta extents", meta_iblock)
}
