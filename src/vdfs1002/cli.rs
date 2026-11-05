use super::{Image, select_file};
use crate::cli::Cli;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;

pub(crate) fn run(image_bytes: &[u8], args: &Cli) -> Result<()> {
    let modes = [args.audit, args.list, args.extract.is_some()]
        .iter()
        .filter(|set| **set)
        .count();
    if modes > 1 {
        bail!("--audit, --list and --extract are mutually exclusive");
    }
    if args.parent.is_some() && !args.list && args.extract.is_none() {
        bail!("--parent needs --list or --extract");
    }
    if modes == 0
        && (!args.include.is_empty() || args.manifest_dir.is_some() || args.aes_key.is_some())
    {
        bail!("--include, --manifest-dir and --aes-key are not supported for VDFS1002 images");
    }

    let reader = Image::open(image_bytes)?;

    if args.audit {
        let records = reader.catalog_records()?;
        let audit = reader.audit(&records)?;
        println!("{}", serde_json::to_string_pretty(&audit.to_json())?);
        return Ok(());
    }

    if args.list {
        let records = reader.catalog_records()?;
        let rows: Vec<&super::CatalogRecord> = records
            .iter()
            .filter(|record| args.parent.is_none_or(|id| record.parent_id == id))
            .collect();
        if args.json {
            let values: Vec<serde_json::Value> =
                rows.iter().map(|record| record.to_json()).collect();
            let listing = serde_json::json!({ "record_count": values.len(), "records": values });
            println!("{}", serde_json::to_string_pretty(&listing)?);
        } else {
            for record in rows {
                println!(
                    "{}\t{}\t{}\t{}",
                    record.parent_id,
                    record.object_id,
                    record.type_name(),
                    String::from_utf8_lossy(&record.name)
                );
            }
        }
        return Ok(());
    }

    if let Some(name) = args.extract.as_deref() {
        let output = args
            .output
            .as_deref()
            .context("--extract requires -o/--output")?;
        let records = reader.catalog_records()?;
        let record = select_file(&records, name, args.parent)?;
        let overflow = reader.overflow_extents()?;
        let contents = reader.file_contents(record, &overflow)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
            .with_context(|| format!("Failed to create output: {}", output.display()))?;
        file.write_all(&contents)?;
        let digest = format!("{:x}", Sha256::digest(&contents));
        let report = serde_json::json!({
            "output": output.display().to_string(),
            "size": contents.len(),
            "sha256": digest,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let output_dir = args.output.clone().unwrap_or_else(|| {
        let stem = args.input.file_stem().unwrap_or_default();
        PathBuf::from(format!("{}-extracted", stem.to_string_lossy()))
    });
    if output_dir.exists() {
        bail!("Output path already exists: {}", output_dir.display());
    }
    println!("VDFS1002 Image");
    println!("Extracting to: {}", output_dir.display());
    let summary = super::restore(image_bytes, &output_dir)?;
    if !summary.extended_checksum_valid {
        eprintln!("  Warning: extended record checksum mismatch");
    }
    println!("  Directories: {}", summary.directories);
    println!("  Files: {}", summary.files);
    println!("  Hard-link names: {}", summary.hardlinks);
    println!("  Bytes written: {}", summary.bytes);
    println!(
        "  Back-references (not created): {}",
        summary.back_references
    );
    println!("  Links not created: {}", summary.links_not_created);
    println!(
        "  Nameless file bodies (hard-link data): {}",
        summary.nameless_bodies
    );
    println!("  Skipped entries: {}", summary.skipped);
    for (object_id, message) in &summary.failures {
        eprintln!("  Failed object {object_id}: {message}");
    }
    if !summary.failures.is_empty() {
        bail!("{} entries failed to restore", summary.failures.len());
    }
    println!("Done!");
    Ok(())
}
