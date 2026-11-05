use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "vdfs-unpack",
    about = "Extract Samsung VDFS4 and VDFS1002 filesystem images"
)]
pub(crate) struct Cli {
    pub(crate) input: PathBuf,

    #[arg(short = 'o', long = "output")]
    pub(crate) output: Option<PathBuf>,

    #[arg(long = "include")]
    pub(crate) include: Vec<String>,

    #[arg(long)]
    pub(crate) manifest_dir: Option<PathBuf>,

    /// File containing the 16-byte raw AES-128 key for encrypted chunks (VDFS4 only).
    #[arg(long = "aes-key")]
    pub(crate) aes_key: Option<PathBuf>,

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
