use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn zero_image(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("vdfs-unpack-{name}-{}.img", std::process::id()));
    fs::write(&path, vec![0u8; 4096]).expect("failed to write test image");
    path
}

fn run(image: &PathBuf, extra: &[&str]) -> Output {
    let out = std::env::temp_dir().join(format!("vdfs-unpack-out-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out);
    Command::new(env!("CARGO_BIN_EXE_vdfs-unpack"))
        .arg(image)
        .arg("-o")
        .arg(&out)
        .args(extra)
        .output()
        .expect("failed to run vdfs-unpack")
}

#[test]
fn unrecognized_image_is_rejected_without_force() {
    let image = zero_image("reject");
    let output = run(&image, &[]);
    let _ = fs::remove_file(&image);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a recognized VDFS1002 or VDFS4 image"));
    assert!(stderr.contains("--force"));
    assert!(!stderr.contains("Bad superblock signature"));
}

#[test]
fn unrecognized_image_is_tried_with_force() {
    let image = zero_image("force");
    let output = run(&image, &["--force"]);
    let _ = fs::remove_file(&image);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("because --force was given"));
    assert!(stderr.contains("Bad superblock signature"));
}
