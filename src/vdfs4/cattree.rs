use anyhow::Result;

use super::btree::Btree;
use super::disk::*;
use super::reader::ImageReader;

pub fn cattree_cmpfn(search: &[u8], stored: &[u8]) -> std::cmp::Ordering {
    if stored.len() < CATTREE_KEY_NAME_OFF || search.len() < CATTREE_KEY_NAME_OFF {
        return std::cmp::Ordering::Less;
    }

    let sp = cattree_key_parent_id(search);
    let ss = cattree_key_parent_id(stored);
    match sp.cmp(&ss) {
        std::cmp::Ordering::Equal => {}
        ord => return ord,
    }

    let sname = cattree_key_name(search);
    let ssname = cattree_key_name(stored);
    let min_len = sname.len().min(ssname.len());
    if min_len > 0 {
        match sname[..min_len].cmp(&ssname[..min_len]) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }
    }

    let slen = cattree_key_name_len(search) as usize;
    let sslen = cattree_key_name_len(stored) as usize;
    match slen.cmp(&sslen) {
        std::cmp::Ordering::Equal => {}
        ord => return ord,
    }

    let so = cattree_key_object_id(search);
    let sso = cattree_key_object_id(stored);
    so.cmp(&sso)
}

fn make_search_key(parent_id: u64, name: &[u8], object_id: u64) -> Vec<u8> {
    let key_len = align8(CATTREE_KEY_NAME_OFF + name.len());
    let mut key = vec![0u8; key_len];

    key[0..4].copy_from_slice(b"catt");
    let kl = key_len as u16;
    key[4..6].copy_from_slice(&kl.to_le_bytes());
    key[6..8].copy_from_slice(&kl.to_le_bytes());

    key[CATTREE_KEY_PARENT_ID_OFF..CATTREE_KEY_PARENT_ID_OFF + 8]
        .copy_from_slice(&parent_id.to_le_bytes());
    key[CATTREE_KEY_OBJECT_ID_OFF..CATTREE_KEY_OBJECT_ID_OFF + 8]
        .copy_from_slice(&object_id.to_le_bytes());

    key[CATTREE_KEY_NAME_LEN_OFF] = name.len() as u8;
    key[CATTREE_KEY_NAME_OFF..CATTREE_KEY_NAME_OFF + name.len()].copy_from_slice(name);

    key
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

pub fn cattree_get_first_child(btree: &Btree, parent_id: u64) -> Result<Vec<Vec<u8>>> {
    let search_key = make_search_key(parent_id, b"", 0);
    let (mut node, mut index) = btree.find_leaf(&cattree_cmpfn, &search_key)?;
    let mut results = Vec::new();

    if index >= node.recs_count as usize {
        if node.next_node_id != 0 {
            node = btree.read_node(node.next_node_id)?;
            index = 0;
        } else {
            return Ok(results);
        }
    }

    loop {
        while index < node.recs_count as usize {
            let rec = node.get_record(index);
            if cattree_key_parent_id(rec) != parent_id {
                return Ok(results);
            }
            results.push(rec.to_vec());
            index += 1;
        }

        if node.next_node_id != 0 {
            node = btree.read_node(node.next_node_id)?;
            index = 0;
        } else {
            break;
        }
    }

    Ok(results)
}

pub fn open_cat_tree<'a>(reader: &'a ImageReader<'a>) -> Result<Btree<'a>> {
    Btree::open(reader, VDFS4_CAT_TREE_INO)
}
