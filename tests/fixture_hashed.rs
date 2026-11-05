use std::fs;
use std::path::PathBuf;
use std::process::Command;

// Built by mkfs.vdfs (hsand/vdfs4-linux) with --sign-all and -h md5, sha1,
// and sha256. Each image holds a descriptor with the matching letter: 'I',
// 'H', and 'h'. The mix of Zip and Lzo covers both codecs. The RSA signing
// key is random, so the images are not byte-reproducible and their hashes are
// not pinned. The tests check the restored file contents instead.
const FIXTURES: [(&str, &str); 3] = [
    ("md5-zlib", "hashed-md5-zlib.vdfs"),
    ("sha1-lzo", "hashed-sha1-lzo.vdfs"),
    ("sha256-lzo", "hashed-sha256-lzo.vdfs"),
];

fn fixture_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(file)
}

/// The same LCG as `long_distance_input` in src/lzo1x.rs, seed 1.
fn random_like_bytes(len: usize) -> Vec<u8> {
    let mut x: u64 = 1;
    (0..len)
        .map(|_| {
            x = (x * 1103515245 + 12345) % (1 << 31);
            (x >> 16) as u8
        })
        .collect()
}

fn extract(name: &str, file: &str) -> PathBuf {
    let out =
        std::env::temp_dir().join(format!("vdfs-unpack-hashed-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out);
    let status = Command::new(env!("CARGO_BIN_EXE_vdfs-unpack"))
        .arg(fixture_path(file))
        .arg("-o")
        .arg(&out)
        .status()
        .expect("failed to run vdfs-unpack");
    assert!(status.success(), "vdfs-unpack exited with {status}");
    out
}

#[test]
fn extracts_hashed_fixture_contents() {
    for (name, file) in FIXTURES {
        let out = extract(name, file);
        let root = out.join("root");

        let expected: [(&str, Vec<u8>); 3] = [
            ("hello.txt", b"hello from the hashed fixture\n".to_vec()),
            ("zeros.bin", vec![0u8; 8192]),
            ("rand.bin", random_like_bytes(60000)),
        ];
        for (path, bytes) in &expected {
            let got = fs::read(root.join(path)).unwrap_or_else(|e| panic!("{name}: {path}: {e}"));
            assert!(got == *bytes, "{name}: content mismatch for {path}");
        }

        fs::remove_dir_all(&out).unwrap();
    }
}
