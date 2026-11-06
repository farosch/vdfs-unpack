use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/small-zlib.vdfs"
);

fn run(name: &str, extra: &[&Path]) -> PathBuf {
    let out = std::env::temp_dir().join(format!(
        "vdfs-unpack-manifest-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&out);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vdfs-unpack"));
    cmd.arg(FIXTURE).arg("-o").arg(&out);
    for path in extra {
        cmd.arg("--manifest-dir").arg(path);
    }
    let status = cmd.status().expect("failed to run vdfs-unpack");
    assert!(status.success(), "vdfs-unpack exited with {status}");
    out
}

fn tree(base: &Path, dir: &Path, acc: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path
            .strip_prefix(base)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let kind = fs::symlink_metadata(&path).unwrap().file_type();
        if kind.is_symlink() {
            acc.push(format!("{rel} -> link"));
        } else if kind.is_dir() {
            acc.push(format!("{rel}/"));
            tree(base, &path, acc);
        } else {
            acc.push(rel);
        }
    }
}

fn listing(root: &Path) -> Vec<String> {
    let mut acc = Vec::new();
    tree(root, root, &mut acc);
    acc.sort();
    acc
}

#[test]
fn manifest_mode_extracts_the_same_tree_as_normal_mode() {
    let normal = run("normal", &[]);
    let manifest_dir =
        std::env::temp_dir().join(format!("vdfs-unpack-mdir-{}", std::process::id()));
    let _ = fs::remove_dir_all(&manifest_dir);
    let manifest = run("manifest", &[&manifest_dir]);

    assert_eq!(listing(&normal), listing(&manifest));
    assert!(listing(&manifest).contains(&"root/link.txt -> link".to_string()));

    let link = manifest.join("root/link.txt");
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("hello.txt"));

    let entries = fs::read_to_string(manifest_dir.join("entries.tsv")).unwrap();
    assert!(
        entries.contains("LINK\troot/link.txt\t68656c6c6f2e747874"),
        "manifest is missing the symlink row: {entries}"
    );

    fs::remove_dir_all(&normal).unwrap();
    fs::remove_dir_all(&manifest).unwrap();
    fs::remove_dir_all(&manifest_dir).unwrap();
}
