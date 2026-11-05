use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};

use super::codec::{compressed_descriptor, decompress_blob};
use super::format::{
    CATALOG_INODE, CatalogRecord, EXTENDED_OFFSET, EXTENDED_SIZE, EXTENT_INODE, EXTENT_KEY_SIZE,
    EXTENT_RECORD_SIZE, Extent, KEY_FIXED_SIZE, MAX_BASE_TABLE_SIZE, MAX_READ_BYTES,
    META_EXTENT_COUNT, NODE_HEADER_SIZE, NODE_INDEX, NODE_LEAF, Node, Overflow, RECORD_FILE,
    SECTOR_SIZE, SPECIAL_INODE_COUNT, SUPERBLOCK_OFFSET, Translation, file_extents, kernel_crc32,
    le_u16, le_u32, le_u64, parse_fork, parse_mode, record, span,
};

pub struct Image<'a> {
    data: &'a [u8],
    block_size: u64,
    node_size: u64,
    meta: Vec<Extent>,
    base_offset: u64,
    translation: Translation,
    declared_blocks: u64,
    /// Informational only, as in the Python reader: a bad extended-record CRC does not reject the image.
    pub extended_checksum_valid: bool,
}

impl<'a> Image<'a> {
    pub fn open(data: &'a [u8]) -> Result<Self> {
        let superblock = span(data, SUPERBLOCK_OFFSET, SECTOR_SIZE)?;
        if &superblock[0..4] != b"VDFS" || &superblock[4..8] != b"1002" {
            bail!("not a VDFS1002 superblock");
        }
        if kernel_crc32(&[&superblock[..508]]) != le_u32(superblock, 508)? {
            bail!("superblock checksum mismatch");
        }
        let log_block = superblock[164];
        let log_super_page = superblock[165];
        if log_block != 12 || log_super_page < log_block || log_super_page >= 32 {
            bail!("unsupported block or super-page order {log_block}/{log_super_page}");
        }

        let extended = span(data, EXTENDED_OFFSET, EXTENDED_SIZE)?;
        let extended_checksum_valid = kernel_crc32(&[&extended[..EXTENDED_SIZE as usize - 4]])
            == le_u32(extended, EXTENDED_SIZE as usize - 4)?;
        let meta = (0..META_EXTENT_COUNT)
            .map(|index| {
                Ok(Extent {
                    begin: le_u64(extended, 88 + 16 * index)?,
                    length: le_u64(extended, 96 + 16 * index)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut image = Image {
            data,
            block_size: 1 << log_block,
            node_size: 1 << log_super_page,
            meta,
            base_offset: 0,
            translation: HashMap::new(),
            declared_blocks: le_u64(superblock, 8)?,
            extended_checksum_valid,
        };
        let (base_offset, translation) = image.load_base_table()?;
        image.base_offset = base_offset;
        image.translation = translation;
        Ok(image)
    }

    fn read(&self, offset: u64, length: u64) -> Result<&'a [u8]> {
        span(self.data, offset, length)
    }

    fn byte_offset(&self, block: u64) -> Result<u64> {
        block
            .checked_mul(self.block_size)
            .with_context(|| format!("block {block} is out of range"))
    }

    fn load_base_table(&self) -> Result<(u64, Translation)> {
        let base_offset = self.byte_offset(self.meta[0].begin)?;
        let header = self.read(base_offset, 24)?;
        if &header[0..4] != b"CoWB" {
            bail!("base table magic not found at the first meta extent");
        }
        let checksum_offset = le_u64(header, 16)?;
        if !(104..=MAX_BASE_TABLE_SIZE).contains(&checksum_offset) {
            bail!("base table checksum offset {checksum_offset} out of range");
        }
        let table = self.read(base_offset, checksum_offset + 4)?;
        let checksum_at = checksum_offset as usize;
        if kernel_crc32(&[&table[..checksum_at]]) != le_u32(table, checksum_at)? {
            bail!("base table checksum mismatch");
        }
        let mut translation = HashMap::new();
        for index in 0..SPECIAL_INODE_COUNT {
            let last_index = le_u64(table, 24 + 8 * index)?;
            let records_offset = le_u64(table, 64 + 8 * index)?;
            translation.insert(index as u64 + 3, (last_index, records_offset));
        }
        Ok((base_offset, translation))
    }

    fn translation_for(&self, inode: u64) -> Result<(u64, u64)> {
        self.translation
            .get(&inode)
            .copied()
            .with_context(|| format!("inode {inode} has no base table entry"))
    }

    fn node_offset(&self, node_id: u32, inode: u64) -> Result<u64> {
        let (last_index, records_offset) = self.translation_for(inode)?;
        if u64::from(node_id) > last_index {
            bail!("node {node_id} is beyond the translation table ({last_index})");
        }
        let entry_offset = self
            .base_offset
            .checked_add(records_offset)
            .and_then(|base| base.checked_add(16 * u64::from(node_id)))
            .context("translation entry offset overflows")?;
        let meta_iblock = le_u64(self.read(entry_offset, 16)?, 0)?;
        let node_extent = self.meta[1];
        if meta_iblock >= node_extent.length {
            bail!("meta_iblock {meta_iblock} is outside the node extent");
        }
        let block = node_extent.begin.saturating_add(meta_iblock);
        let offset = self.byte_offset(block)?;
        if offset
            .checked_add(self.node_size)
            .is_none_or(|end| end > self.data.len() as u64)
        {
            bail!("node {node_id} at block {block} extends past the image");
        }
        Ok(offset)
    }

    fn read_node(&self, node_id: u32, inode: u64) -> Result<Node<'a>> {
        let raw = self.read(self.node_offset(node_id, inode)?, self.node_size)?;
        let node_type = le_u32(raw, 28)?;
        if node_type != NODE_INDEX && node_type != NODE_LEAF {
            bail!("node {node_id} has type {node_type}");
        }
        let count = le_u16(raw, 14)? as usize;
        if count > raw.len() / 8 {
            bail!("node {node_id} record count {count} is impossible");
        }
        let mut offsets = Vec::with_capacity(count + 1);
        for index in 0..=count {
            let address = raw.len() - 4 * (index + 2);
            let offset = le_u32(raw, address)? as usize;
            if offset < NODE_HEADER_SIZE || offset >= raw.len() {
                bail!("node {node_id} record {index} offset {offset} is outside the node");
            }
            offsets.push(offset);
        }
        Ok(Node {
            id: node_id,
            node_type,
            next_node: le_u32(raw, 24)?,
            offsets,
            data: raw,
        })
    }

    fn head(&self, inode: u64) -> Result<(u32, u16)> {
        let raw = self.read(self.node_offset(0, inode)?, self.node_size)?;
        if &raw[0..4] != b"eHND" {
            bail!("head magic not found for inode {inode}");
        }
        let root = le_u32(raw, 12)?;
        let height = le_u16(raw, 16)?;
        if height < 1 {
            bail!("inode {inode} tree height is zero");
        }
        Ok((root, height))
    }

    fn leaf_records(&self, inode: u64, key_minimum: usize) -> Result<Vec<(u32, usize, &'a [u8])>> {
        let (root, height) = self.head(inode)?;
        let mut node = self.read_node(root, inode)?;
        for _ in 1..height {
            if node.node_type != NODE_INDEX || node.offsets.len() < 2 {
                bail!("node {} is not a valid index node", node.id);
            }
            let child = record(&node, 0, KEY_FIXED_SIZE)?;
            let pointer = le_u16(child, 4)? as usize;
            node = self.read_node(le_u32(child, pointer)?, inode)?;
        }
        if node.node_type != NODE_LEAF {
            bail!("inode {inode} descent did not end at a leaf");
        }

        let max_nodes = self.translation_for(inode)?.0 + 1;
        let mut seen = HashSet::new();
        let mut records = Vec::new();
        loop {
            for index in 0..node.offsets.len() - 1 {
                records.push((node.id, index, record(&node, index, key_minimum)?));
            }
            if node.next_node == 0 {
                return Ok(records);
            }
            if seen.contains(&node.next_node) || seen.len() as u64 >= max_nodes {
                bail!("inode {inode} leaf chain loops or exceeds the translation table");
            }
            seen.insert(node.id);
            node = self.read_node(node.next_node, inode)?;
        }
    }

    pub fn catalog_records(&self) -> Result<Vec<CatalogRecord>> {
        let mut records = Vec::new();
        for (node_id, index, bytes) in self.leaf_records(CATALOG_INODE, KEY_FIXED_SIZE)? {
            let key_length = le_u16(bytes, 4)? as usize;
            let name_length = bytes[25] as usize;
            if KEY_FIXED_SIZE + name_length > key_length {
                bail!("node {node_id} record {index} name overruns its key");
            }
            let record_type = bytes[24];
            let (size, blocks, extents) = if record_type == RECORD_FILE {
                parse_fork(node_id, index, bytes, key_length)?
            } else {
                (0, 0, Vec::new())
            };
            records.push(CatalogRecord {
                leaf_node: node_id,
                parent_id: le_u64(bytes, 8)?,
                object_id: le_u64(bytes, 16)?,
                record_type,
                name: bytes[KEY_FIXED_SIZE..KEY_FIXED_SIZE + name_length].to_vec(),
                mode: parse_mode(bytes, key_length),
                size,
                blocks,
                extents,
            });
        }
        Ok(records)
    }

    pub fn overflow_extents(&self) -> Result<Overflow> {
        let mut owners: Overflow = HashMap::new();
        for (node_id, index, bytes) in self.leaf_records(EXTENT_INODE, EXTENT_KEY_SIZE)? {
            if bytes.len() != EXTENT_RECORD_SIZE {
                bail!(
                    "node {node_id} record {index} has length {}, expected {EXTENT_RECORD_SIZE}",
                    bytes.len()
                );
            }
            let length = le_u64(bytes, 32)?;
            if length == 0 {
                continue;
            }
            owners.entry(le_u64(bytes, 8)?).or_default().push((
                le_u64(bytes, 16)?,
                Extent {
                    begin: le_u64(bytes, 24)?,
                    length,
                },
            ));
        }
        for items in owners.values_mut() {
            items.sort_by_key(|(iblock, _)| *iblock);
        }
        Ok(owners)
    }

    pub fn read_file(
        &self,
        record: &CatalogRecord,
        overflow: &Overflow,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        if record.size > max_bytes {
            bail!(
                "file size {} exceeds the read limit {max_bytes}",
                record.size
            );
        }
        let data_limit = self.meta[1].begin;
        let mut remaining = record.size;
        let mut out = Vec::with_capacity(record.size as usize);
        for extent in file_extents(record, overflow)? {
            if extent.begin.saturating_add(extent.length) > data_limit {
                bail!(
                    "extent {}+{} reaches the metadata area",
                    extent.begin,
                    extent.length
                );
            }
            let take = remaining.min(extent.length.saturating_mul(self.block_size));
            if take == 0 {
                continue;
            }
            out.extend_from_slice(self.read(self.byte_offset(extent.begin)?, take)?);
            remaining -= take;
        }
        if remaining != 0 {
            bail!("extents are {remaining} bytes short of the declared size");
        }
        Ok(out)
    }

    /// Logical file contents, inflating a trailing compressed-file descriptor when present.
    pub fn file_contents(&self, record: &CatalogRecord, overflow: &Overflow) -> Result<Vec<u8>> {
        let blob = self.read_file(record, overflow, MAX_READ_BYTES)?;
        match compressed_descriptor(&blob)? {
            None => Ok(blob),
            Some(descriptor) => decompress_blob(&blob, &descriptor),
        }
    }

    /// Summarise how far the catalog reaches and whether file forks agree with their sizes.
    pub fn audit(&self, records: &[CatalogRecord]) -> Result<Audit> {
        let files: Vec<&CatalogRecord> = records
            .iter()
            .filter(|record| record.record_type == RECORD_FILE)
            .collect();
        let mut fork_consistent = 0;
        let mut max_inline_block_end = 0;
        for record in &files {
            let total: u64 = record.extents.iter().map(|extent| extent.length).sum();
            let fits = record.blocks == 0
                || ((record.blocks - 1).saturating_mul(self.block_size) < record.size
                    && record.size <= record.blocks.saturating_mul(self.block_size));
            if total == record.blocks && fits {
                fork_consistent += 1;
            }
            for extent in &record.extents {
                max_inline_block_end =
                    max_inline_block_end.max(extent.begin.saturating_add(extent.length));
            }
        }
        let overflow = self.overflow_extents()?;
        let files_resolved = files
            .iter()
            .filter(|record| file_extents(record, &overflow).is_ok())
            .count() as u64;
        Ok(Audit {
            declared_blocks: self.declared_blocks,
            image_blocks: self.data.len() as u64 / self.block_size,
            meta_extent_end: self.meta[1].begin.saturating_add(self.meta[1].length),
            max_node_block_end: self.max_node_block_end()?,
            file_records: files.len() as u64,
            fork_consistent,
            max_inline_block_end,
            overflow_objects: overflow.len() as u64,
            overflow_extents: overflow.values().map(|items| items.len() as u64).sum(),
            files_resolved,
        })
    }

    fn max_node_block_end(&self) -> Result<u64> {
        let node_blocks = self.node_size / self.block_size;
        let mut end = 0;
        for &(last_index, records_offset) in self.translation.values() {
            for node_id in 0..=last_index {
                let entry_offset = self
                    .base_offset
                    .checked_add(records_offset)
                    .and_then(|base| base.checked_add(16 * node_id))
                    .context("translation entry offset overflows")?;
                let block = le_u64(self.read(entry_offset, 8)?, 0)?;
                end = end.max(
                    self.meta[1]
                        .begin
                        .saturating_add(block)
                        .saturating_add(node_blocks),
                );
            }
        }
        Ok(end)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Audit {
    pub declared_blocks: u64,
    pub image_blocks: u64,
    pub meta_extent_end: u64,
    pub max_node_block_end: u64,
    pub file_records: u64,
    pub fork_consistent: u64,
    pub max_inline_block_end: u64,
    pub overflow_objects: u64,
    pub overflow_extents: u64,
    pub files_resolved: u64,
}

impl Audit {
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "declared_blocks": self.declared_blocks,
            "image_blocks": self.image_blocks,
            "meta_extent_end": self.meta_extent_end,
            "max_node_block_end": self.max_node_block_end,
            "file_records": self.file_records,
            "fork_consistent": self.fork_consistent,
            "max_inline_block_end": self.max_inline_block_end,
            "overflow_objects": self.overflow_objects,
            "overflow_extents": self.overflow_extents,
            "files_resolved": self.files_resolved,
        })
    }
}
