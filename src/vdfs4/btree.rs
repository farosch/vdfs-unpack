use anyhow::{Context, Result, bail};

use super::disk::*;
use super::reader::ImageReader;

pub struct Btree<'a> {
    pub reader: &'a ImageReader<'a>,
    pub tree_ino: u64,
    pub root_node_id: u32,
    pub height: u16,
    pub node_size: usize,
}

pub struct Bnode {
    pub node_id: u32,
    pub next_node_id: u32,
    pub node_type: u32,
    pub recs_count: u16,
    pub offsets: Vec<u32>,
    pub data: Vec<u8>,
}

impl Bnode {
    pub fn get_record(&self, index: usize) -> &[u8] {
        let off = self.offsets[index] as usize;
        let available = self.data.len() - off;
        if available < 8 {
            return &self.data[off..off + available];
        }
        let rec = &self.data[off..];
        let rec_len = cattree_key_record_len(rec).min(available);
        &rec[..rec_len]
    }

    pub fn get_child_node_id(&self, index: usize) -> u32 {
        let rec = self.get_record(index);
        let key_len = cattree_key_key_len(rec);
        if key_len > 0 && key_len + 4 <= rec.len() {
            read_u32_le(rec, key_len)
        } else {
            0
        }
    }
}

impl<'a> Btree<'a> {
    pub fn open(reader: &'a ImageReader<'a>, tree_ino: u64) -> Result<Self> {
        let node_size = reader.sbi.super_page_size();
        let meta_page = reader.read_meta_page(tree_ino, 0)?;

        let head: Vdfs4RawBtreeHead =
            unsafe { read_packed(&meta_page, 0).context("Failed to read btree head")? };

        if head.magic != *VDFS4_BTREE_HEAD_NODE_MAGIC {
            bail!(
                "Invalid btree head magic for ino {}: {:?}",
                tree_ino,
                head.magic
            );
        }

        Ok(Self {
            reader,
            tree_ino,
            root_node_id: head.root_bnode_id,
            height: head.btree_height,
            node_size,
        })
    }

    pub fn read_node(&self, node_id: u32) -> Result<Bnode> {
        let raw = if node_id == 0 {
            self.reader.read_meta_page(self.tree_ino, 0)?
        } else {
            self.reader.read_bnode_data(self.tree_ino, node_id)?
        };

        if raw.len() < 32 {
            bail!("Node {} too small ({} bytes)", node_id, raw.len());
        }

        let node_type = read_u32_le(&raw, 28);
        let recs_count = read_u16_le(&raw, 14) as usize;
        let next_node_id = read_u32_le(&raw, 24);

        let mut offsets = Vec::with_capacity(recs_count + 1);
        for i in 0..=recs_count {
            let addr = self.node_size - VDFS4_BNODE_FIRST_OFFSET - 4 * (i + 1);
            let off = read_u32_le(&raw, addr) as usize;
            offsets.push(off as u32);
        }

        Ok(Bnode {
            node_id,
            next_node_id,
            node_type,
            recs_count: recs_count as u16,
            offsets,
            data: raw,
        })
    }

    pub fn get_first_leaf_node(&self) -> Result<Bnode> {
        let mut node = self.read_node(self.root_node_id)?;

        for _level in 0..self.height - 1 {
            if node.recs_count == 0 {
                bail!("Empty internal node {}", node.node_id);
            }
            let child_id = node.get_child_node_id(0);
            if child_id == 0 {
                bail!("Null child at index 0 in node {}", node.node_id);
            }
            node = self.read_node(child_id)?;
        }

        Ok(node)
    }

    pub fn collect_all_records(&self) -> Result<Vec<Vec<u8>>> {
        let mut node = self.get_first_leaf_node()?;
        let mut records = Vec::new();
        loop {
            for i in 0..node.recs_count as usize {
                records.push(node.get_record(i).to_vec());
            }
            if node.next_node_id == 0 {
                break;
            }
            node = self.read_node(node.next_node_id)?;
        }
        Ok(records)
    }

    pub fn find_leaf(
        &self,
        cmp_fn: &dyn Fn(&[u8], &[u8]) -> std::cmp::Ordering,
        search_key: &[u8],
    ) -> Result<(Bnode, usize)> {
        let mut node = self.read_node(self.root_node_id)?;

        for _level in 0..self.height {
            if node.recs_count == 0 {
                bail!("Empty node {} at level", node.node_id);
            }

            let (diff, index) = binary_search(&node, cmp_fn, search_key);

            if node.node_type == VDFS4_NODE_LEAF {
                return Ok((node, index));
            }

            let child_index = if diff == 0 {
                index
            } else if index > 0 {
                index - 1
            } else {
                0
            };
            let child_id = node.get_child_node_id(child_index);
            if child_id == 0 {
                bail!(
                    "Null child node at index {} in node {}",
                    child_index,
                    node.node_id
                );
            }
            node = self.read_node(child_id)?;
        }

        bail!("Reached bottom of tree without finding leaf")
    }
}

pub fn binary_search(
    node: &Bnode,
    cmp_fn: &dyn Fn(&[u8], &[u8]) -> std::cmp::Ordering,
    search_key: &[u8],
) -> (i32, usize) {
    let mut low: usize = 0;
    let mut high: usize = node.recs_count as usize;
    let mut diff: i32 = -1;

    while low < high {
        let mid = low + (high - low) / 2;
        let record = node.get_record(mid);
        let ord = cmp_fn(search_key, record);
        match ord {
            std::cmp::Ordering::Equal => {
                return (0, mid);
            }
            std::cmp::Ordering::Greater => {
                diff = 1;
                low = mid + 1;
            }
            std::cmp::Ordering::Less => {
                diff = -1;
                high = mid;
            }
        }
    }

    (diff, low)
}
