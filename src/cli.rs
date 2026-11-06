use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "vdfs-unpack",
    about = "Extract Samsung VDFS4 and VDFS1002 filesystem images"
)]
pub(crate) struct Cli {
    /// Path to the VDFS image (VDFS4 or VDFS1002).
    pub(crate) input: PathBuf,

    /// Directory to extract into. Defaults to `<image-stem>-extracted` in the current directory.
    #[arg(short = 'o', long = "output")]
    pub(crate) output: Option<PathBuf>,

    /// Extract only this image path (repeatable). Without it the whole image is extracted.
    #[arg(long = "include")]
    pub(crate) include: Vec<String>,

    /// VDFS4: write manifest files (catalog and xattrs) into this directory.
    #[arg(long)]
    pub(crate) manifest_dir: Option<PathBuf>,

    /// File containing the 16-byte raw AES-128 key for encrypted chunks (VDFS4 only).
    #[arg(long = "aes-key")]
    pub(crate) aes_key: Option<PathBuf>,

    /// Try VDFS4 extraction on an image that is not a recognized VDFS1002 or VDFS4 image.
    #[arg(long)]
    pub(crate) force: bool,

    /// VDFS1002: print the catalog records instead of restoring.
    #[arg(long)]
    pub(crate) list: bool,

    /// VDFS1002: with --list, print the records as JSON.
    #[arg(long, requires = "list")]
    pub(crate) json: bool,

    /// VDFS1002: print block-reach and file-fork audit as JSON.
    #[arg(long)]
    pub(crate) audit: bool,

    /// VDFS1002: write the file record with this name to -o (a new file).
    #[arg(long)]
    pub(crate) extract: Option<String>,

    /// VDFS1002: with --list or --extract, only match records whose parent object id is this.
    #[arg(long)]
    pub(crate) parent: Option<u64>,
}
