use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/small-zlib-aes.vdfs"
);
const FIXTURE_SHA256: &str = "f71096dde0844afe286dc8d2677a15c0f328122fdb3f817ce66370a8d90629bb";
// Synthetic key shared with tests/fixtures/regenerate.sh. Not a real secret.
const TEST_KEY: &[u8; 16] = b"0123456789abcdef";
const WRONG_KEY: &[u8; 16] = b"fedcba9876543210";

fn write_key(name: &str, key: &[u8; 16]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("vdfs-unpack-{name}-{}.key", std::process::id()));
    fs::write(&path, key).expect("failed to write key file");
    path
}

fn run_extract(name: &str, key: Option<&Path>) -> (Output, PathBuf) {
    let out = std::env::temp_dir().join(format!("vdfs-unpack-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vdfs-unpack"));
    cmd.arg(FIXTURE).arg("-o").arg(&out);
    if let Some(key) = key {
        cmd.arg("--aes-key").arg(key);
    }
    (cmd.output().expect("failed to run vdfs-unpack"), out)
}

#[test]
fn encrypted_fixture_matches_recorded_sha256() {
    let output = Command::new("sha256sum")
        .arg(FIXTURE)
        .output()
        .expect("sha256sum is required for this test");
    let line = String::from_utf8_lossy(&output.stdout);
    assert!(
        line.starts_with(FIXTURE_SHA256),
        "fixture changed; regenerate it and update DEVELOP.md: {line}"
    );
}

#[test]
fn decrypts_fixture_with_correct_key() {
    let key = write_key("correct", TEST_KEY);
    let (output, out) = run_extract("enc-correct", Some(&key));
    assert!(
        output.status.success(),
        "vdfs-unpack failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let root = out.join("root");
    let expected: [(&str, Vec<u8>); 3] = [
        ("hello.txt", b"hello from the vdfs fixture\n".to_vec()),
        ("zeros.bin", vec![0u8; 8192]),
        ("sub/deeper/n.txt", b"nested\n".to_vec()),
    ];
    for (path, bytes) in &expected {
        let got = fs::read(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(&got, bytes, "content mismatch for {path}");
    }

    fs::remove_dir_all(&out).unwrap();
    fs::remove_file(&key).unwrap();
}

#[test]
fn encrypted_chunks_without_key_are_rejected() {
    let (output, out) = run_extract("enc-nokey", None);
    assert!(
        !output.status.success(),
        "extraction without a key must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--aes-key"), "unexpected error: {stderr}");
    let _ = fs::remove_dir_all(&out);
}

#[test]
fn wrong_key_is_rejected() {
    let key = write_key("wrong", WRONG_KEY);
    let (output, out) = run_extract("enc-wrong", Some(&key));
    assert!(
        !output.status.success(),
        "wrong key must not extract cleanly"
    );
    let _ = fs::remove_dir_all(&out);
    fs::remove_file(&key).unwrap();
}
