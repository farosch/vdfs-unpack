use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const S_IFMT: u16 = 0xF000;
const S_IFLNK: u16 = 0xA000;

use super::btree::Btree;
use super::cattree::{cattree_get_first_child, open_cat_tree};
use super::decompress::decompress_file;
use super::disk::*;
use super::extents::{
    build_extent_overflow_map, extents_read_data, extents_resolve_fork, open_ext_tree,
};
use super::reader::ImageReader;

type OverflowMap = HashMap<u64, Vec<(u64, Vdfs4Extent)>>;

fn checked_name(record: &[u8]) -> Result<String> {
    let name = std::str::from_utf8(cattree_key_name(record))?;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(char::is_control)
    {
        bail!("Unsafe catalog name");
    }
    Ok(name.to_owned())
}

pub fn validate_selection(includes: &[String]) -> Result<()> {
    if includes.len() > 1024 {
        bail!("Too many selected paths");
    }
    for path in includes {
        if path.len() > 4096
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            bail!("Unsafe selected path");
        }
    }
    Ok(())
}

fn selected_path(path: &str, includes: &[String]) -> bool {
    includes.is_empty()
        || includes.iter().any(|allowed| {
            path == allowed
                || allowed.starts_with(&format!("{}/", path))
                || path.starts_with(&format!("{}/", allowed))
        })
}

pub fn unpack_filesystem(
    reader: &ImageReader,
    output_dir: &Path,
    includes: &[String],
) -> Result<()> {
    validate_selection(includes)?;
    let cat_tree = open_cat_tree(reader)?;

    fs::create_dir_all(output_dir).context("Failed to create output directory")?;

    // Build an index from object_id -> catalog record (file or folder record
    // carrying the real metadata). Used to resolve internal/hard links.
    let all_records = cat_tree.collect_all_records()?;
    let mut by_object: HashMap<u64, Vec<u8>> = HashMap::new();
    for rec in &all_records {
        let rtype = cattree_key_record_type(rec);
        if rtype == VDFS4_CATALOG_FILE_RECORD || rtype == VDFS4_CATALOG_FOLDER_RECORD {
            let oid = cattree_key_object_id(rec);
            by_object.insert(oid, rec.clone());
        }
    }

    // Resolve overflow extents once (files with > 9 extents keep the rest in
    // the extents tree).
    let overflow = build_extent_overflow_map(reader, &open_ext_tree(reader)?)?;

    // Symlinks are created only after every regular file is written, so no
    // later write can follow a link planted by the image.
    let mut symlinks = Vec::new();
    let ctx = WalkCtx {
        reader,
        cat_tree: &cat_tree,
        by_object: &by_object,
        overflow: &overflow,
        includes,
    };
    process_directory(
        &ctx,
        VDFS4_ROOTDIR_OBJ_ID,
        output_dir,
        &mut HashSet::new(),
        "",
        &mut symlinks,
    )?;

    for (link_path, target) in &symlinks {
        create_symlink(link_path, target)?;
    }

    Ok(())
}

fn create_symlink(link_path: &Path, target: &str) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(link_path) {
        if meta.is_dir() {
            bail!(
                "Cannot replace directory with symlink: {}",
                link_path.display()
            );
        }
        fs::remove_file(link_path)
            .with_context(|| format!("remove conflicting file: {}", link_path.display()))?;
    }
    symlink_unix(target, link_path)
        .with_context(|| format!("Failed to create symlink: {}", link_path.display()))?;
    println!("SYMLINK\t{}\t{:?}", link_path.display(), target);
    Ok(())
}

#[cfg(unix)]
fn symlink_unix(target: &str, link_path: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link_path)
}

#[cfg(not(unix))]
fn symlink_unix(_target: &str, _link_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symbolic links can only be restored on Unix",
    ))
}

/// Read-only state shared by every directory visited during one unpack.
struct WalkCtx<'a> {
    reader: &'a ImageReader<'a>,
    cat_tree: &'a Btree<'a>,
    by_object: &'a HashMap<u64, Vec<u8>>,
    overflow: &'a OverflowMap,
    includes: &'a [String],
}

fn process_directory(
    ctx: &WalkCtx,
    dir_id: u64,
    output_path: &Path,
    visited: &mut HashSet<u64>,
    relative_path: &str,
    symlinks: &mut Vec<(PathBuf, String)>,
) -> Result<()> {
    let WalkCtx {
        reader,
        cat_tree,
        by_object,
        overflow,
        includes,
    } = ctx;
    if !visited.insert(dir_id) {
        return Ok(());
    }

    let children =
        cattree_get_first_child(cat_tree, dir_id).context("Failed to get first child")?;

    for child_rec in children {
        let record_type = cattree_key_record_type(&child_rec);
        // ILINK rows are reverse-parent index entries, not listed children (the kernel's readdir skips them).
        if record_type == VDFS4_CATALOG_ILINK_RECORD {
            if super::manifest::enabled() {
                super::manifest::internal_record(&child_rec, output_path)?;
            }
            continue;
        }
        let name = checked_name(&child_rec)?.to_string();
        let relative = if relative_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", relative_path, name)
        };
        if !selected_path(&relative, includes) {
            continue;
        }

        super::manifest::record(&child_rec, by_object, &output_path.join(&name))?;

        match record_type {
            VDFS4_CATALOG_FOLDER_RECORD => {
                let object_id = cattree_key_object_id(&child_rec);
                let child_path = output_path.join(&name);
                if child_path.is_file() {
                    fs::remove_file(&child_path).with_context(|| {
                        format!("remove conflicting file: {}", child_path.display())
                    })?;
                }
                fs::create_dir_all(&child_path)
                    .with_context(|| format!("Failed to create dir: {}", child_path.display()))?;
                process_directory(ctx, object_id, &child_path, visited, &relative, symlinks)?;
            }
            VDFS4_CATALOG_FILE_RECORD => {
                let object_id = cattree_key_object_id(&child_rec);
                let key_len = cattree_key_key_len(&child_rec);
                let value_data = &child_rec[key_len..];
                let child_path = output_path.join(&name);
                extract_file(
                    reader,
                    overflow,
                    object_id,
                    value_data,
                    &child_path,
                    symlinks,
                )?;
            }
            VDFS4_CATALOG_HLINK_RECORD => {
                resolve_and_extract(
                    reader,
                    by_object,
                    overflow,
                    &child_rec,
                    output_path,
                    symlinks,
                )?;
            }
            _ => {
                bail!("Unknown record type {} for {}", record_type, name);
            }
        }
    }

    Ok(())
}

fn resolve_and_extract(
    reader: &ImageReader,
    by_object: &HashMap<u64, Vec<u8>>,
    overflow: &OverflowMap,
    link_rec: &[u8],
    output_path: &Path,
    symlinks: &mut Vec<(PathBuf, String)>,
) -> Result<()> {
    let link_object = cattree_key_object_id(link_rec);
    let name = checked_name(link_rec)?.to_string();
    let child_path = output_path.join(&name);

    // The link's object_id field is the target file's object_id. Look it up in
    // the index built from the real file records.
    match by_object.get(&link_object) {
        Some(target_rec) => {
            let target_type = cattree_key_record_type(target_rec);
            let key_len = cattree_key_key_len(target_rec);
            let target_value = &target_rec[key_len..];

            match target_type {
                VDFS4_CATALOG_FILE_RECORD => {
                    let target_object = cattree_key_object_id(target_rec);
                    extract_file(
                        reader,
                        overflow,
                        target_object,
                        target_value,
                        &child_path,
                        symlinks,
                    )?;
                }
                VDFS4_CATALOG_FOLDER_RECORD => {
                    // Directory hard link: create the directory entry but do
                    // not recurse, to avoid duplicating the target subtree
                    // (the real folder is extracted separately).
                    if child_path.is_file() {
                        fs::remove_file(&child_path).with_context(|| {
                            format!("remove conflicting file: {}", child_path.display())
                        })?;
                    }
                    fs::create_dir_all(&child_path).with_context(|| {
                        format!("Failed to create dir: {}", child_path.display())
                    })?;
                }
                _ => bail!("Unsupported link target type"),
            }
        }
        None => {
            bail!("Could not resolve link {} (obj={})", name, link_object);
        }
    }

    Ok(())
}

fn extract_file(
    reader: &ImageReader,
    overflow: &OverflowMap,
    object_id: u64,
    value_data: &[u8],
    output_path: &Path,
    symlinks: &mut Vec<(PathBuf, String)>,
) -> Result<()> {
    if value_data.len() < std::mem::size_of::<Vdfs4CatalogFileRecord>() {
        bail!("Truncated file catalog record: {}", output_path.display());
    }

    let file_rec: Vdfs4CatalogFileRecord =
        unsafe { std::ptr::read_unaligned(value_data.as_ptr() as *const Vdfs4CatalogFileRecord) };

    let fork = file_rec.fork;
    let flags = file_rec.common.flags;
    let file_mode = file_rec.common.file_mode;
    let file_size = fork.size_in_bytes;

    // Symbolic link: the target path is stored inline (fast symlink) or in
    // extents (page symlink). Extract it as an actual symlink so that
    // e.g. `bin -> usr/bin` is recreated as a link rather than a regular file.
    if (file_mode & S_IFMT) == S_IFLNK {
        let target = if fork.total_blocks_count == 0 {
            let inline = fork.inline_data();
            let len = (file_size as usize).min(inline.len());
            String::from_utf8_lossy(&inline[..len]).to_string()
        } else {
            let extents = extents_resolve_fork(&fork, file_size, overflow, object_id)?;
            let data = extents_read_data(reader, &extents, 0, file_size)?;
            String::from_utf8_lossy(&data).to_string()
        };
        let target = target.trim_end_matches('\0').to_string();
        if super::manifest::enabled() {
            super::manifest::symlink(output_path, target.as_bytes())?;
        }
        symlinks.push((output_path.to_path_buf(), target));
        return Ok(());
    }

    if file_size == 0 {
        fs::write(output_path, b"")
            .with_context(|| format!("write (zero) failed: {}", output_path.display()))?;
        println!("FILE\t{}\t0\t{}", output_path.display(), object_id);
        return Ok(());
    }

    // Handle name collision: if a directory exists at this path (e.g. from a
    // hard link resolving to a folder), remove it before writing the file.
    if output_path.is_dir() {
        if is_dir_empty(output_path) {
            fs::remove_dir_all(output_path)
                .with_context(|| format!("remove conflicting dir: {}", output_path.display()))?;
        } else {
            bail!(
                "File collides with non-empty directory: {}",
                output_path.display()
            );
        }
    } else if output_path.is_file() {
        fs::remove_file(output_path)
            .with_context(|| format!("remove conflicting file: {}", output_path.display()))?;
    }

    if super::decompress::is_compressed_file(flags) {
        let data = decompress_file(reader, &fork, object_id, overflow)?;
        fs::write(output_path, &data)
            .with_context(|| format!("write (compressed) failed: {}", output_path.display()))?;
    } else if fork.total_blocks_count == 0 {
        let inline = fork.inline_data();
        let len = usize::try_from(file_size)?;
        if len > inline.len() {
            bail!("Inline file size exceeds fork");
        }
        fs::write(output_path, &inline[..len])
            .with_context(|| format!("write (inline) failed: {}", output_path.display()))?;
    } else {
        let extents = extents_resolve_fork(&fork, file_size, overflow, object_id)?;
        let data = extents_read_data(reader, &extents, 0, file_size)?;
        fs::write(output_path, &data)
            .with_context(|| format!("write (extents) failed: {}", output_path.display()))?;
    }

    println!(
        "FILE\t{}\t{}\t{}",
        output_path.display(),
        fs::metadata(output_path)?.len(),
        object_id
    );
    Ok(())
}

fn is_dir_empty(path: &Path) -> bool {
    match fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_record(name: &[u8]) -> Vec<u8> {
        let mut record = vec![0; CATTREE_KEY_NAME_OFF + name.len()];
        record[CATTREE_KEY_NAME_LEN_OFF] = name.len() as u8;
        record[CATTREE_KEY_NAME_OFF..].copy_from_slice(name);
        record
    }

    #[test]
    fn safe_names_remain_single_components() {
        assert_eq!(
            checked_name(&name_record(b"SWUCoreTV")).unwrap(),
            "SWUCoreTV"
        );
        for name in [
            b"".as_slice(),
            b".",
            b"..",
            b"/etc",
            b"../etc",
            b"a/b",
            b"a\\b",
            b"a\nb",
            b"\xff",
        ] {
            assert!(checked_name(&name_record(name)).is_err());
        }
    }

    #[test]
    fn selection_retains_ancestors_and_selected_subtrees_only() {
        let includes = vec![
            "root/usr/bin/SWUCoreTV".to_string(),
            "root/usr/share/swu".to_string(),
        ];
        validate_selection(&includes).unwrap();
        for path in [
            "root",
            "root/usr",
            "root/usr/bin",
            "root/usr/bin/SWUCoreTV",
            "root/usr/share/swu",
            "root/usr/share/swu/key.txt",
        ] {
            assert!(selected_path(path, &includes));
        }
        for path in [
            "root/usr/apps",
            "root/usr/bin/SWUCoreTV-other",
            "root/usr/share/swu-other",
        ] {
            assert!(!selected_path(path, &includes));
        }
    }

    #[test]
    fn selection_rejects_unsafe_components() {
        for path in [
            "",
            "/root",
            "root/../etc",
            "root/./etc",
            "root//etc",
            "root\\etc",
            "root/line\nbreak",
        ] {
            assert!(validate_selection(&[path.to_string()]).is_err());
        }
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vdfs-unpack-unit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_symlink_replaces_existing_file() {
        let dir = scratch_dir("symlink-replace");
        let link = dir.join("bin");
        fs::write(&link, b"old").unwrap();

        create_symlink(&link, "usr/bin").unwrap();

        assert_eq!(fs::read_link(&link).unwrap(), Path::new("usr/bin"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn create_symlink_refuses_to_replace_directory() {
        let dir = scratch_dir("symlink-dir");
        let link = dir.join("bin");
        fs::create_dir(&link).unwrap();

        assert!(create_symlink(&link, "usr/bin").is_err());
        assert!(link.is_dir());
        fs::remove_dir_all(&dir).unwrap();
    }
}
