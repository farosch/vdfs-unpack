use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::format::{
    CatalogRecord, RECORD_FILE, RECORD_FOLDER, RECORD_HARDLINK, RECORD_INDIRECT_LINK, S_IFLNK,
    S_IFMT,
};
use super::image::Image;

#[derive(Debug, Default)]
pub struct RestoreSummary {
    pub extended_checksum_valid: bool,
    pub directories: u64,
    pub files: u64,
    pub hardlinks: u64,
    pub bytes: u64,
    pub back_references: u64,
    pub links_not_created: u64,
    pub nameless_bodies: u64,
    pub skipped: u64,
    pub failures: Vec<(u64, String)>,
}

/// Restore folders, file bodies and hard-link names under a new `destination`.
///
/// An indirect record (type 5) is an upward back-reference, so it creates nothing.
/// Hard-link names are copies of the body, not filesystem hard links.
pub fn restore_tree(
    image: &Image<'_>,
    records: &[CatalogRecord],
    destination: &Path,
) -> Result<RestoreSummary> {
    if destination.exists() {
        bail!(
            "refusing to restore into existing path {}",
            destination.display()
        );
    }
    let overflow = image.overflow_extents()?;
    let folders = by_object(records, RECORD_FOLDER);
    let bodies = by_object(records, RECORD_FILE);
    let mut summary = RestoreSummary::default();
    fs::create_dir_all(destination)?;

    let mut folder_modes = Vec::new();
    for folder in folders.values() {
        match relative_parts(&folders, folder.parent_id) {
            Some(parts) if all_safe(&parts) && safe(&folder.name) => {
                let path = join(destination, &parts, &folder.name);
                fs::create_dir_all(&path)?;
                if let Some(mode) = folder.mode {
                    folder_modes.push((path, folder.object_id, mode));
                }
                summary.directories += 1;
            }
            _ => summary.skipped += 1,
        }
    }

    for record in records {
        let (body, hardlink) = match record.record_type {
            RECORD_FILE if is_nameless_body(record) => {
                summary.nameless_bodies += 1;
                continue;
            }
            RECORD_FILE => (record, false),
            RECORD_HARDLINK => match bodies.get(&record.object_id) {
                Some(body) => (*body, true),
                None => {
                    summary.links_not_created += 1;
                    continue;
                }
            },
            RECORD_INDIRECT_LINK => {
                if is_back_reference(record, &bodies, &folders) {
                    summary.back_references += 1;
                } else {
                    summary.links_not_created += 1;
                }
                continue;
            }
            _ => continue,
        };
        let parts = match relative_parts(&folders, record.parent_id) {
            Some(parts) if all_safe(&parts) && safe(&record.name) => parts,
            _ => {
                summary.skipped += 1;
                continue;
            }
        };
        let contents = match image.file_contents(body, &overflow) {
            Ok(contents) => contents,
            Err(error) => {
                summary
                    .failures
                    .push((record.object_id, format!("{error:#}")));
                continue;
            }
        };
        let target = join(destination, &parts, &record.name);
        let link = body.mode.is_some_and(|mode| mode & S_IFMT == S_IFLNK);
        let written = if link {
            create_symlink(&target, &contents)
        } else {
            write_new(&target, &contents)
        };
        if let Err(error) = written {
            summary
                .failures
                .push((record.object_id, format!("write failed: {error}")));
            continue;
        }
        if let Some(mode) = body.mode.filter(|_| !link)
            && let Err(error) = set_mode(&target, mode)
        {
            summary
                .failures
                .push((record.object_id, format!("chmod failed: {error}")));
        }
        if hardlink {
            summary.hardlinks += 1;
        } else {
            summary.files += 1;
        }
        summary.bytes += contents.len() as u64;
    }
    for (path, object_id, mode) in folder_modes {
        if let Err(error) = set_mode(&path, mode) {
            summary
                .failures
                .push((object_id, format!("chmod failed: {error}")));
        }
    }
    Ok(summary)
}

/// Open `image`, decode its catalog and restore the tree under `destination`.
pub fn restore(image: &[u8], destination: &Path) -> Result<RestoreSummary> {
    let opened = Image::open(image)?;
    let records = opened.catalog_records()?;
    let mut summary = restore_tree(&opened, &records, destination)?;
    summary.extended_checksum_valid = opened.extended_checksum_valid;
    Ok(summary)
}

/// The only file record named `name`, optionally restricted to one parent object.
pub fn select_file<'r>(
    records: &'r [CatalogRecord],
    name: &str,
    parent: Option<u64>,
) -> Result<&'r CatalogRecord> {
    let candidates: Vec<&CatalogRecord> = records
        .iter()
        .filter(|record| {
            record.record_type == RECORD_FILE
                && record.name == name.as_bytes()
                && parent.is_none_or(|id| record.parent_id == id)
        })
        .collect();
    match candidates.as_slice() {
        [record] => Ok(record),
        _ => bail!(
            "{} file records named {name:?}; narrow with --parent",
            candidates.len()
        ),
    }
}

fn by_object(records: &[CatalogRecord], record_type: u8) -> BTreeMap<u64, &CatalogRecord> {
    records
        .iter()
        .filter(|record| record.record_type == record_type)
        .map(|record| (record.object_id, record))
        .collect()
}

/// Names of the folders above `parent_id`, outermost first. `None` marks a cycle or an orphan.
fn relative_parts(
    folders: &BTreeMap<u64, &CatalogRecord>,
    mut parent_id: u64,
) -> Option<Vec<Vec<u8>>> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    while let Some(node) = folders.get(&parent_id) {
        if !seen.insert(parent_id) {
            return None;
        }
        chain.push(node.name.clone());
        parent_id = node.parent_id;
    }
    if parent_id != 0 && chain.is_empty() {
        return None;
    }
    chain.reverse();
    Some(chain)
}

fn is_back_reference(
    record: &CatalogRecord,
    bodies: &BTreeMap<u64, &CatalogRecord>,
    folders: &BTreeMap<u64, &CatalogRecord>,
) -> bool {
    let parent = bodies
        .get(&record.parent_id)
        .or_else(|| folders.get(&record.parent_id));
    parent.is_some_and(|parent| parent.name == record.name && parent.parent_id == record.object_id)
}

/// A file record with an empty name that is its own parent holds data only: hard-link
/// names read it through the object id, so it has no path of its own.
fn is_nameless_body(record: &CatalogRecord) -> bool {
    record.name.is_empty() && record.parent_id == record.object_id
}

fn safe(name: &[u8]) -> bool {
    !matches!(name, b"" | b"." | b"..") && !name.contains(&b'/') && !name.contains(&0)
}

fn all_safe(parts: &[Vec<u8>]) -> bool {
    parts.iter().all(|part| safe(part))
}

#[cfg(unix)]
fn bytes_to_os(bytes: &[u8]) -> OsString {
    std::ffi::OsStr::from_bytes(bytes).to_os_string()
}

/// Non-Unix paths are not arbitrary bytes, so names are decoded lossily.
#[cfg(not(unix))]
fn bytes_to_os(bytes: &[u8]) -> OsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}

fn join(destination: &Path, parts: &[Vec<u8>], name: &[u8]) -> PathBuf {
    let mut path = destination.to_path_buf();
    for part in parts {
        path.push(bytes_to_os(part));
    }
    path.push(bytes_to_os(name));
    path
}

fn write_new(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    file.write_all(contents)
}

/// Apply the stored permission bits exactly, without the process umask.
#[cfg(unix)]
fn set_mode(target: &Path, mode: u16) -> std::io::Result<()> {
    fs::set_permissions(target, fs::Permissions::from_mode(u32::from(mode & 0o7777)))
}

/// Permission bits have no direct equivalent on non-Unix filesystems, so they are not applied.
#[cfg(not(unix))]
fn set_mode(_target: &Path, _mode: u16) -> std::io::Result<()> {
    Ok(())
}

/// A symbolic link's body holds its target path. Links are made only after every
/// folder exists, so no later write can pass through one.
#[cfg(unix)]
fn create_symlink(target: &Path, link_target: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(bytes_to_os(link_target), target)
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _link_target: &[u8]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symbolic links can only be restored on Unix",
    ))
}
