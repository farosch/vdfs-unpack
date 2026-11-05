use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/small-zlib.vdfs"
);
const FIXTURE_SHA256: &str = "9c2dc702e85f4a3741f7aa9ed56673fd91fc7142a791e38763c8e9fb91a14f1f";

fn extract_fixture(name: &str) -> PathBuf {
    let out = std::env::temp_dir().join(format!("vdfs-unpack-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out);
    let status = Command::new(env!("CARGO_BIN_EXE_vdfs-unpack"))
        .arg(FIXTURE)
        .arg("-o")
        .arg(&out)
        .status()
        .expect("failed to run vdfs-unpack");
    assert!(status.success(), "vdfs-unpack exited with {status}");
    out
}

#[test]
fn fixture_matches_recorded_sha256() {
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
fn extracts_fixture_file_contents() {
    let out = extract_fixture("contents");
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
}

#[test]
fn extracted_files_are_exactly_the_expected_set() {
    let out = extract_fixture("set");
    let mut files = Vec::new();
    collect_files(&out.join("root"), &out.join("root"), &mut files);
    files.sort();
    assert_eq!(
        files,
        ["hello.txt", "link.txt", "sub/deeper/n.txt", "zeros.bin"],
    );

    fs::remove_dir_all(&out).unwrap();
}

#[test]
fn symlink_is_created_with_recorded_target() {
    let out = extract_fixture("symlink");
    let link = out.join("root/link.txt");
    let meta = fs::symlink_metadata(&link).expect("link.txt missing");
    assert!(meta.file_type().is_symlink(), "link.txt is not a symlink");
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("hello.txt"));
    assert_eq!(
        fs::read(&link).unwrap(),
        b"hello from the vdfs fixture\n",
        "symlink does not resolve to hello.txt"
    );

    fs::remove_dir_all(&out).unwrap();
}

#[test]
fn extracted_directories_are_exactly_the_expected_set() {
    let out = extract_fixture("dirs");
    let mut dirs = Vec::new();
    collect_dirs(&out.join("root"), &out.join("root"), &mut dirs);
    dirs.sort();
    assert_eq!(dirs, ["sub", "sub/deeper"]);

    fs::remove_dir_all(&out).unwrap();
}

fn collect_files(base: &Path, dir: &Path, acc: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(base, &path, acc);
        } else {
            let rel = path.strip_prefix(base).unwrap();
            acc.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn collect_dirs(base: &Path, dir: &Path, acc: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            let rel = path.strip_prefix(base).unwrap();
            acc.push(rel.to_string_lossy().replace('\\', "/"));
            collect_dirs(base, &path, acc);
        }
    }
}
