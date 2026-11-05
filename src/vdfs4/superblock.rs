use anyhow::{Context, Result, bail};

use super::disk::*;

pub struct SuperblockInfo {
    pub layout: Vdfs4LayoutSb,
    pub block_size: usize,
    pub block_size_shift: u32,
    pub log_super_page_size: u32,
    pub log_blocks_in_leb: u32,
    pub volume_blocks_count: u64,
    pub case_insensitive: bool,
}

impl SuperblockInfo {
    pub fn super_page_size(&self) -> usize {
        1 << self.log_super_page_size as usize
    }

    pub fn blocks_per_super_page(&self) -> usize {
        1 << self.log_blocks_in_leb as usize
    }
}

pub fn read_superblock(data: &[u8]) -> Result<SuperblockInfo> {
    if data.len() < SECTOR_SIZE * 8 {
        bail!("Image too small for superblock");
    }

    let sb_offset = SECTOR_SIZE * 2;

    let sb: Vdfs4SuperBlock =
        unsafe { read_packed(data, sb_offset).context("Failed to read superblock")? };

    if sb.signature != *VDFS4_SB_SIGNATURE {
        let sig = String::from_utf8_lossy(&sb.signature);
        bail!("Bad superblock signature: {:?} (expected VDFS)", sig);
    }

    let layout_ver = &sb.layout_version;
    if layout_ver != VDFS4_LAYOUT_VERSION_2007 && layout_ver != VDFS4_LAYOUT_VERSION_2006 {
        let ver = String::from_utf8_lossy(layout_ver);
        bail!("Unsupported layout version: {}", ver);
    }

    if sb.image_crc32_present != 0 {
        let calculated = crc32fast::hash(&data[sb_offset..sb_offset + SECTOR_SIZE - 4]);
        let sb_checksum = sb.checksum;
        if calculated != sb_checksum {
            bail!(
                "Superblock CRC mismatch: stored={:#x}, calculated={:#x}",
                sb_checksum,
                calculated
            );
        }
    }

    let block_size = 1u32 << sb.log_block_size;
    if !matches!(block_size, 512 | 1024 | 2048 | 4096) {
        bail!("Unsupported block size: {}", block_size);
    }

    if sb.log_super_page_size < sb.log_block_size {
        bail!(
            "Super page order ({}) < block size order ({})",
            sb.log_super_page_size,
            sb.log_block_size
        );
    }

    let exsb_offset = SECTOR_SIZE * 3;
    let exsb_size = SECTOR_SIZE * 5;
    let exsb: Vdfs4ExtendedSuperBlock =
        unsafe { read_packed(data, exsb_offset).context("Failed to read extended superblock")? };

    if sb.image_crc32_present != 0 {
        let exsb_calc = crc32fast::hash(&data[exsb_offset..exsb_offset + exsb_size - 4]);
        let exsb_checksum = exsb.checksum;
        if exsb_calc != exsb_checksum {
            bail!(
                "Extended SB CRC mismatch: stored={:#x}, calculated={:#x}",
                exsb_checksum,
                exsb_calc
            );
        }
    }

    let layout: Vdfs4LayoutSb =
        unsafe { read_packed(data, 0).context("Failed to read full layout superblock")? };

    let info = SuperblockInfo {
        layout,
        block_size: block_size as usize,
        block_size_shift: sb.log_block_size as u32,
        log_super_page_size: sb.log_super_page_size as u32,
        log_blocks_in_leb: sb.log_super_page_size as u32 - sb.log_block_size as u32,
        volume_blocks_count: exsb.volume_blocks_count,
        case_insensitive: sb.case_insensitive != 0,
    };

    Ok(info)
}
