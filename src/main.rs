mod cli;
mod hex;
mod lzo1x;
mod vdfs1002;
mod vdfs4;

use anyhow::{Context, Result, bail};
use clap::Parser;

fn main() -> Result<()> {
    let args = cli::Cli::parse();

    if !args.input.exists() {
        bail!("Input file does not exist: {}", args.input.display());
    }

    let image = std::fs::read(&args.input)
        .with_context(|| format!("Failed to read image: {}", args.input.display()))?;

    if vdfs1002::is_vdfs1002(&image) {
        return vdfs1002::cli::run(&image, &args);
    }
    vdfs4::run(&image, &args)
}
