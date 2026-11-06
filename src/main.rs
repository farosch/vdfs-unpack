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
    if !vdfs4::is_vdfs4(&image) {
        if !args.force {
            bail!(
                "{} is not a recognized VDFS1002 or VDFS4 image (use --force to try VDFS4 anyway)",
                args.input.display()
            );
        }
        eprintln!(
            "warning: {} is not a recognized VDFS1002 or VDFS4 image; trying VDFS4 because --force was given",
            args.input.display()
        );
    }
    vdfs4::run(&image, &args)
}
