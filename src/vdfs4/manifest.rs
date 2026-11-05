use super::btree::Btree;
use super::disk::*;
use super::reader::ImageReader;
use crate::hex;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

thread_local! {
    static ACTIVE: RefCell<Option<Manifest>> = const { RefCell::new(None) };
}
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 200_000;

struct Manifest {
    root: PathBuf,
    table: BufWriter<File>,
    records: BufWriter<File>,
    record_bytes: u64,
    table_bytes: u64,
    entries: usize,
    paths: HashSet<String>,
}

pub fn host_component(name: &[u8], object_id: u64) -> Result<String> {
    if name.is_empty()
        || name.len() > 255
        || name == b"."
        || name == b".."
        || name.contains(&b'/')
        || name.contains(&0)
    {
        bail!("Invalid original catalog name");
    }
    if name
        .iter()
        .all(|value| value.is_ascii_alphanumeric() || b"._-".contains(value))
    {
        return Ok(std::str::from_utf8(name)?.to_owned());
    }
    Ok(format!(
        "~vdfs-unpack-{:016x}-{}",
        object_id,
        hex::encode(&Sha256::digest(name))
    ))
}

fn checked_record(record: &[u8]) -> Result<()> {
    if record.len() < CATTREE_KEY_NAME_OFF {
        bail!("Truncated manifest catalog key");
    }
    let key = cattree_key_key_len(record);
    if key < CATTREE_KEY_NAME_OFF
        || key > record.len()
        || cattree_key_record_len(record) != record.len()
        || CATTREE_KEY_NAME_OFF + cattree_key_name_len(record) as usize > key
    {
        bail!("Invalid manifest catalog bounds");
    }
    Ok(())
}

pub fn enabled() -> bool {
    ACTIVE.with(|active| active.borrow().is_some())
}

pub fn mapped_name(record: &[u8]) -> Result<String> {
    checked_record(record)?;
    host_component(cattree_key_name(record), cattree_key_object_id(record))
}

pub fn start(directory: Option<&Path>, root: &Path) -> Result<()> {
    ACTIVE.with(|active| -> Result<()> {
        let mut slot = active.borrow_mut();
        if slot.is_some() {
            bail!("Manifest already active");
        }
        if let Some(directory) = directory {
            std::fs::create_dir_all(directory)?;
            let table = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join("entries.tsv"))?;
            let records = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join("catalog.bin"))?;
            let mut manifest = Manifest {
                root: root.to_owned(),
                table: BufWriter::new(table),
                records: BufWriter::new(records),
                record_bytes: 0,
                table_bytes: 0,
                entries: 0,
                paths: HashSet::new(),
            };
            manifest.row("VDFS-UNPACK-MANIFEST\t1".to_owned())?;
            *slot = Some(manifest);
        }
        Ok(())
    })
}

impl Manifest {
    fn row(&mut self, row: String) -> Result<()> {
        self.table_bytes = self
            .table_bytes
            .checked_add(row.len() as u64 + 1)
            .context("Manifest size overflow")?;
        if self.table_bytes > MAX_BYTES {
            bail!("Manifest table exceeds 64 MiB limit");
        }
        writeln!(self.table, "{}", row)?;
        Ok(())
    }

    fn relative(&self, path: &Path) -> Result<String> {
        let relative = path
            .strip_prefix(&self.root)?
            .to_str()
            .context("Non-ASCII host path")?;
        if relative.is_empty()
            || relative.len() > 4096
            || relative.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || !part
                        .bytes()
                        .all(|value| value.is_ascii_alphanumeric() || b"._-~".contains(&value))
            })
        {
            bail!("Unsafe manifest host path");
        }
        Ok(relative.to_owned())
    }

    fn append(&mut self, record: &[u8]) -> Result<u64> {
        checked_record(record)?;
        let offset = self.record_bytes;
        self.record_bytes = offset
            .checked_add(record.len() as u64)
            .context("Catalog size overflow")?;
        if self.record_bytes > MAX_BYTES {
            bail!("Manifest catalog exceeds 64 MiB limit");
        }
        self.records.write_all(record)?;
        Ok(offset)
    }
}

pub fn record(record: &[u8], objects: &HashMap<u64, Vec<u8>>, path: &Path) -> Result<()> {
    ACTIVE.with(|active| -> Result<()> {
        let mut slot = active.borrow_mut();
        if let Some(manifest) = slot.as_mut() {
            checked_record(record)?;
            let relative = manifest.relative(path)?;
            let duplicate = !manifest.paths.insert(relative.clone());
            let preexisting = path.symlink_metadata().is_ok();
            if duplicate || preexisting {
                bail!(
                    "Manifest host-path collision at {} (duplicate={} preexisting={})",
                    relative,
                    duplicate,
                    preexisting
                );
            }
            manifest.entries += 1;
            if manifest.entries > MAX_ENTRIES {
                bail!("Manifest exceeds 200000 catalog records");
            }
            let resolved = match cattree_key_record_type(record) {
                VDFS4_CATALOG_FILE_RECORD | VDFS4_CATALOG_FOLDER_RECORD => record,
                VDFS4_CATALOG_ILINK_RECORD | VDFS4_CATALOG_HLINK_RECORD => objects
                    .get(&cattree_key_object_id(record))
                    .context("Missing manifest link target")?
                    .as_slice(),
                _ => bail!("Unknown manifest catalog record type"),
            };
            checked_record(resolved)?;
            if cattree_key_object_id(record) != cattree_key_object_id(resolved) {
                bail!("Manifest link identity mismatch");
            }
            let own_offset = manifest.append(record)?;
            let resolved_offset = if resolved == record {
                own_offset
            } else {
                manifest.append(resolved)?
            };
            manifest.row(format!(
                "ENTRY\t{}\t{}\t{}\t{}\t{}",
                relative,
                own_offset,
                record.len(),
                resolved_offset,
                resolved.len()
            ))?;
        }
        Ok(())
    })
}

pub fn internal_record(record: &[u8], parent: &Path) -> Result<()> {
    ACTIVE.with(|active| -> Result<()> {
        let mut slot = active.borrow_mut();
        let manifest = slot.as_mut().context("Manifest not active")?;
        checked_record(record)?;
        if cattree_key_record_type(record) != VDFS4_CATALOG_ILINK_RECORD {
            bail!("Expected internal catalog link");
        }
        let parent_path = if parent == manifest.root {
            String::new()
        } else {
            manifest.relative(parent)?
        };
        manifest.entries += 1;
        if manifest.entries > MAX_ENTRIES {
            bail!("Manifest exceeds 200000 catalog records");
        }
        let offset = manifest.append(record)?;
        manifest.row(format!(
            "INTERNAL\t{}\t{}\t{}",
            parent_path,
            offset,
            record.len()
        ))
    })
}

pub fn symlink(path: &Path, target: &[u8]) -> Result<()> {
    ACTIVE.with(|active| -> Result<()> {
        let mut slot = active.borrow_mut();
        let manifest = slot.as_mut().context("Manifest not active")?;
        let relative = manifest.relative(path)?;
        if target.len() > 65536 || !manifest.paths.contains(&relative) {
            bail!("Invalid manifest symlink target");
        }
        manifest.row(format!("LINK\t{}\t{}", relative, hex::encode(target)))
    })
}

pub fn finish() -> Result<()> {
    ACTIVE.with(|active| -> Result<()> {
        if let Some(mut manifest) = active.borrow_mut().take() {
            manifest.row(format!(
                "END\t{}\t{}",
                manifest.entries, manifest.record_bytes
            ))?;
            manifest.records.flush()?;
            manifest.table.flush()?;
        }
        Ok(())
    })
}

fn checked_xattr_record(record: &[u8]) -> Result<()> {
    if record.len() < 18 {
        bail!("Truncated xattr record");
    }
    let key = read_u16_le(record, 4) as usize;
    let length = read_u16_le(record, 6) as usize;
    let name = record[16] as usize;
    if key < 17 || key > length || length != record.len() || name == 0 || 17 + name > key {
        bail!(
            "Invalid xattr record bounds (key_bytes={} record_bytes={} available={} object={} name_bytes={})",
            key,
            length,
            record.len(),
            read_u64_le(record, 8),
            name
        );
    }
    if key == length {
        if read_u64_le(record, 8) != 0 || record[17..17 + name].contains(&0) {
            bail!("Invalid valueless xattr key");
        }
        return Ok(());
    }
    if record[17..17 + name].contains(&0) || key + 1 + record[key] as usize > length {
        bail!("Invalid xattr name/value bounds");
    }
    Ok(())
}

pub fn export_xattrs(reader: &ImageReader, directory: &Path) -> Result<()> {
    let tree = Btree::open(reader, VDFS4_XATTR_TREE_INO)?;
    let mut node = tree.get_first_leaf_node()?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("xattrs.bin"))?;
    let mut output = BufWriter::new(file);
    output.write_all(b"OSEAX01\0")?;
    output.write_all(&0u64.to_le_bytes())?;
    let mut visited = HashSet::new();
    let mut count = 0u64;
    let mut total = 16u64;
    loop {
        if !visited.insert(node.node_id)
            || visited.len() > MAX_ENTRIES
            || node.node_type != VDFS4_NODE_LEAF
        {
            bail!("Invalid or cyclic xattr leaf chain");
        }
        for index in 0..node.recs_count as usize {
            let record = node.get_record(index);
            checked_xattr_record(record)?;
            count += 1;
            total = total
                .checked_add(12 + record.len() as u64)
                .context("Xattr export size overflow")?;
            if count > MAX_ENTRIES as u64 || total > MAX_BYTES - 16 {
                bail!("Xattr export exceeds record/byte limit");
            }
            output.write_all(&node.node_id.to_le_bytes())?;
            output.write_all(&(index as u32).to_le_bytes())?;
            output.write_all(&(record.len() as u32).to_le_bytes())?;
            output.write_all(record)?;
        }
        if node.next_node_id == 0 {
            break;
        }
        node = tree.read_node(node.next_node_id)?;
    }
    output.write_all(b"ENDXATTR")?;
    output.write_all(&count.to_le_bytes())?;
    output.seek(SeekFrom::Start(8))?;
    output.write_all(&count.to_le_bytes())?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserved_aliases_preserve_valid_byte_names_without_host_controls() {
        assert_eq!(host_component(b"SWUCoreTV", 1).unwrap(), "SWUCoreTV");
        let alias = host_component(b"\r", 53110).unwrap();
        assert!(alias.starts_with("~vdfs-unpack-000000000000cf76-"));
        assert!(alias.len() < 255);
        assert_ne!(host_component(alias.as_bytes(), 53110).unwrap(), alias);
        assert_ne!(
            host_component(b"\r", 1).unwrap(),
            host_component(b"\r", 2).unwrap()
        );
        for name in [b"".as_slice(), b".", b"..", b"a/b", b"a\0b"] {
            assert!(host_component(name, 1).is_err());
        }
        assert!(host_component(b"\xff", 1).is_ok());
        assert!(host_component(&[255; 255], 1).unwrap().len() < 255);
    }

    #[test]
    fn xattr_length_prefix_and_key_bounds_are_checked() {
        let mut record = vec![0; 48];
        record[4..6].copy_from_slice(&32u16.to_le_bytes());
        record[6..8].copy_from_slice(&48u16.to_le_bytes());
        record[16] = 4;
        record[17..21].copy_from_slice(b"test");
        record[32] = 3;
        record[33..36].copy_from_slice(b"val");
        assert!(checked_xattr_record(&record).is_ok());
        for length in [0, 17, 47] {
            assert!(checked_xattr_record(&record[..length]).is_err());
        }
        record[32] = 255;
        assert!(checked_xattr_record(&record).is_err());
        let mut boundary = vec![0; 32];
        boundary[4..6].copy_from_slice(&32u16.to_le_bytes());
        boundary[6..8].copy_from_slice(&32u16.to_le_bytes());
        boundary[16] = 4;
        boundary[17..21].copy_from_slice(b"test");
        assert!(checked_xattr_record(&boundary).is_ok());
        boundary[8] = 1;
        assert!(checked_xattr_record(&boundary).is_err());
    }
}
