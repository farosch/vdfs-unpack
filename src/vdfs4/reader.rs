use anyhow::{Context, Result, bail};

use super::disk::*;
use super::snapshot::SnapshotInfo;
use super::snapshot::{get_meta_iblock, meta_iblock_to_block};
use super::superblock::SuperblockInfo;

pub struct ImageReader<'a> {
    pub image: &'a [u8],
    pub sbi: SuperblockInfo,
    pub snapshot: SnapshotInfo,
    /// 16-byte AES-128 key for encrypted chunks; `None` if no key was supplied.
    pub aes_key: Option<[u8; 16]>,
}

impl<'a> ImageReader<'a> {
    pub fn new(image: &'a [u8]) -> Result<Self> {
        let sbi = super::superblock::read_superblock(image)?;
        let snapshot = super::snapshot::build_snapshot_manager(&sbi, image)?;
        Ok(Self {
            image,
            sbi,
            snapshot,
            aes_key: None,
        })
    }

    pub fn read_meta_page(&self, ino: u64, page_index: usize) -> Result<Vec<u8>> {
        let meta_iblock = get_meta_iblock(&self.snapshot, &self.sbi, ino, page_index)
            .context("get_meta_iblock failed")?;

        let block_nr =
            meta_iblock_to_block(&self.sbi, meta_iblock).context("meta_iblock_to_block failed")?;

        let byte_offset = block_nr as usize * self.sbi.block_size;
        let node_size = self.sbi.block_size * self.sbi.blocks_per_super_page();

        if byte_offset + node_size > self.image.len() {
            bail!(
                "Meta page out of bounds: block={}, byte_offset={}, node_size={}",
                block_nr,
                byte_offset,
                node_size
            );
        }

        Ok(self.image[byte_offset..byte_offset + node_size].to_vec())
    }

    pub fn read_bnode_data(&self, tree_ino: u64, node_id: u32) -> Result<Vec<u8>> {
        if node_id == 0 {
            return self.read_meta_page(tree_ino, 0);
        }

        let blocks_per_node = self.sbi.blocks_per_super_page();
        let pages_per_node = blocks_per_node * self.sbi.block_size / 4096;

        let node_page = node_id as usize * pages_per_node;
        self.read_meta_page(tree_ino, node_page)
    }

    pub fn read_extent_data(&self, extent: &Vdfs4Extent) -> Result<Vec<u8>> {
        if extent.is_empty() {
            return Ok(Vec::new());
        }
        let begin = extent.begin;
        let length = extent.length;
        if length > self.sbi.volume_blocks_count + 16 {
            bail!(
                "Extent length too large: begin={}, length={}",
                begin,
                length
            );
        }
        let offset = match begin
            .checked_mul(self.sbi.block_size as u64)
            .and_then(|v| usize::try_from(v).ok())
        {
            Some(v) => v,
            None => bail!("Extent begin too large: begin={}", begin),
        };
        let len = match length
            .checked_mul(self.sbi.block_size as u64)
            .and_then(|v| usize::try_from(v).ok())
        {
            Some(v) => v,
            None => bail!("Extent length too large: length={}", length),
        };
        if offset
            .checked_add(len)
            .map(|end| end > self.image.len())
            .unwrap_or(true)
        {
            bail!("Extent out of bounds: begin={}, length={}", begin, length);
        }
        Ok(self.image[offset..offset + len].to_vec())
    }
}
