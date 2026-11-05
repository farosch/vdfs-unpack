//! Read-only VDFS1002 reader for Samsung HawkM `platform.img`.
//!
//! Independent VDFS1002 reader. The superblock, base table and catalog
//! B-tree (special inode 3) are decoded, file forks are resolved through their
//! inline and overflow extents, and the tree is restored under a new directory.
//! Two address rules are specific to HawkM: the base table sits at the first
//! meta extent's begin block, and each meta_iblock is a block offset from the
//! second meta extent's begin block. The overflow extent tree (inode 5) follows
//! the VDFS4 reference and is unverified on HawkM, where that tree is empty.
//! Lzo compressed descriptors are supported. Zstd compressed descriptors are rejected.

pub(crate) mod cli;
mod codec;
mod format;
mod image;
mod restore;
#[cfg(test)]
mod tests;

pub use format::CatalogRecord;
pub use image::Image;
pub use restore::{restore, select_file};

use format::SUPERBLOCK_OFFSET;

/// Only the first eight bytes at offset 0x400 are checked when detecting a VDFS1002 image.
pub fn is_vdfs1002(image: &[u8]) -> bool {
    image.get(SUPERBLOCK_OFFSET as usize..SUPERBLOCK_OFFSET as usize + 8) == Some(b"VDFS1002")
}
