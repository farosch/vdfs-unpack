mod btree;
mod cattree;
mod decompress;
mod disk;
mod extents;
mod manifest;
mod reader;
mod snapshot;
mod superblock;
mod unpacker;

use crate::cli::Cli;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

/// Checks the superblock signature and layout version that `superblock::read_superblock` requires.
pub(crate) fn is_vdfs4(image: &[u8]) -> bool {
    let sb = disk::SECTOR_SIZE * 2;
    let signature = image.get(sb..sb + 4);
    let layout = image.get(sb + 4..sb + 8);
    signature == Some(disk::VDFS4_SB_SIGNATURE.as_slice())
        && (layout == Some(disk::VDFS4_LAYOUT_VERSION_2006.as_slice())
            || layout == Some(disk::VDFS4_LAYOUT_VERSION_2007.as_slice()))
}

pub(crate) fn run(image: &[u8], args: &Cli) -> Result<()> {
    if args.audit || args.list || args.extract.is_some() || args.parent.is_some() {
        bail!("--audit, --list, --extract and --parent are only supported for VDFS1002 images");
    }

    let output_dir = args.output.clone().unwrap_or_else(|| {
        let stem = args.input.file_stem().unwrap_or_default();
        PathBuf::from(format!("{}-extracted", stem.to_string_lossy()))
    });

    let mut reader = reader::ImageReader::new(image)?;
    if let Some(key_path) = args.aes_key.as_deref() {
        let raw = std::fs::read(key_path)
            .with_context(|| format!("Failed to read AES key: {}", key_path.display()))?;
        let key: [u8; 16] = raw
            .as_slice()
            .try_into()
            .with_context(|| format!("AES key must be exactly 16 bytes: {}", key_path.display()))?;
        reader.aes_key = Some(key);
    }

    println!("VDFS4 Image Info:");
    println!("  Block size: {} bytes", reader.sbi.block_size);
    println!("  Volume blocks: {}", reader.sbi.volume_blocks_count);
    println!("  Case insensitive: {}", reader.sbi.case_insensitive);

    println!("Extracting to: {}", output_dir.display());
    manifest::start(args.manifest_dir.as_deref(), &output_dir)?;
    if let Some(directory) = args.manifest_dir.as_deref() {
        manifest::export_xattrs(&reader, directory)?;
    }
    unpacker::unpack_filesystem(&reader, &output_dir, &args.include)?;
    manifest::finish()?;

    println!("Done!");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_vdfs4;

    fn image_with(signature: &[u8], layout: &[u8]) -> Vec<u8> {
        let mut image = vec![0u8; 2048];
        image[1024..1028].copy_from_slice(signature);
        image[1028..1032].copy_from_slice(layout);
        image
    }

    #[test]
    fn detects_vdfs4_layouts() {
        assert!(is_vdfs4(&image_with(b"VDFS", b"2007")));
        assert!(is_vdfs4(&image_with(b"VDFS", b"2006")));
    }

    #[test]
    fn rejects_other_images() {
        assert!(!is_vdfs4(&image_with(b"VDFS", b"1002")));
        assert!(!is_vdfs4(&image_with(b"XXXX", b"2007")));
        assert!(!is_vdfs4(&[0u8; 2048]));
        assert!(!is_vdfs4(&[0u8; 100]));
    }
}
