use std::fs;
use std::path::PathBuf;
use std::process::Command;

// Built from the same tree as the hashed fixtures: hello.txt, zeros.bin, rand.bin
// (60 KB LCG bytes, seed 1), and sub/deeper/n.txt. The lzo and zst images come
// from mkfs.vdfs (hsand/vdfs4-linux) with -c lzo and -c zstd. The zhw image comes
// from upstream vdfs-tools mkfs.vdfs with a -q list that compresses zeros.bin.
// Each image holds a descriptor whose codec tag is the one named in the file
// name. These are synthetic, so they show the codec path works, not that a
// real Samsung image uses it.
const FIXTURES: [(&str, &str, &[u8]); 3] = [
    ("lzo", "codec-lzo.vdfs", b"Lzo"),
    ("zst", "codec-zst.vdfs", b"Zst"),
    ("zhw", "codec-zhw.vdfs", b"Zhw"),
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
    let out = std::env::temp_dir().join(format!("vdfs-unpack-codec-{name}-{}", std::process::id()));
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
fn extracts_codec_fixture_contents() {
    for (name, file, tag) in FIXTURES {
        let image = fs::read(fixture_path(file)).unwrap();
        assert!(
            image.windows(3).any(|w| w == tag),
            "{name}: image has no {} descriptor",
            String::from_utf8_lossy(tag)
        );

        let out = extract(name, file);
        let root = out.join("root");

        let expected: [(&str, Vec<u8>); 4] = [
            ("hello.txt", b"hello from the vdfs fixture\n".to_vec()),
            ("zeros.bin", vec![0u8; 8192]),
            ("rand.bin", random_like_bytes(60000)),
            ("sub/deeper/n.txt", b"nested\n".to_vec()),
        ];
        for (path, bytes) in &expected {
            let got = fs::read(root.join(path)).unwrap_or_else(|e| panic!("{name}: {path}: {e}"));
            assert!(got == *bytes, "{name}: content mismatch for {path}");
        }

        fs::remove_dir_all(&out).unwrap();
    }
}
