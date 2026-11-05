# Developing vdfs-unpack

## Test and build

```sh
cargo test --locked                  # all tests (75 unit + 1 ignored survey + 4 encrypted + 5 zlib + 1 hashed + 1 codec fixture)
cargo build --release --locked       # ./target/release/vdfs-unpack
cargo fmt --check                    # formatting (rustup component add rustfmt)
cargo clippy --locked --all-targets  # lints (rustup component add clippy)
```

The compressed-descriptor survey is ignored by default because it needs a real image. Run it with
`VDFS1002_SURVEY_IMAGE=<image> cargo test --release --locked -- --ignored survey_compressed_descriptors --nocapture`.
It lists the outcome of every file body and fails on any read, CRC, descriptor or decode error.

Always pass `--locked`; `Cargo.lock` is pinned. CI runs `cargo test --locked`
on pull requests to `master` (`.github/workflows/test.yml`).

Coding rules (module layout, error handling, tests, comments) are in
[CONVENTIONS.md](CONVENTIONS.md).

## Layout

- `src/main.rs`: entry point. Dispatches to VDFS1002 or VDFS4 by image magic.
- `src/cli.rs`: the shared `Cli` argument struct.
- `src/lzo1x.rs`: LZO1X codec, shared by VDFS4 and VDFS1002.
- `src/hex.rs`: shared hex helpers (`encode`; `decode` is test-only).
- `src/vdfs4/mod.rs`: the VDFS4 pipeline (`run`). Submodules are listed below.
- `src/vdfs4/decompress.rs`: compressed-file decoding.
  - Layout 2007 (`find_descriptor`, `decompress_blob`): the descriptor magic's
    first byte selects the digest (`DigestAlgo::from_magic`): `C` none, `I`
    MD5, `H` SHA-1, `h` SHA-256. Chunk digests are checked in extent-table
    order, then the descriptor digest (computed with bytes 24..28 zeroed).
  - Layout 2006 (`decompress_layout5`): chunk hashes are checked; descriptor
    hash and signature are not.
  - Codecs: `Zip`, `Gzp`, and `Zst` (VDFS4 only) plus `Lzo`. `Lzo` is raw
    LZO1X in `src/lzo1x.rs`, a wrapper over the `lzo` crate. Each chunk is
    bounded by the bytes still expected for the file. Tests use liblzo2
    reference streams. Zst is still rejected for VDFS1002.
- `src/vdfs1002/`: VDFS1002 reader (HawkM `platform.img`, `data_backup.img`).
  `mod.rs` re-exports the pieces: `format.rs` (on-disk structs and constants),
  `image.rs` (`Image`, catalog and fork access), `codec.rs` (descriptors and
  chunk decoding), `restore.rs` (tree restore), `tests.rs` (unit tests).
  `codec.rs` accepts descriptor markers `C` and `I`. `locate_chunk_table` finds
  the 16-byte `XT` table by the descriptor CRC, searching back up to
  `COMPRESSED_MAX_TABLE_GAP` (64 KiB) for padding before the descriptor.
  Written for this crate from the VDFS1002 format. `main.rs` dispatches to it
  when the image starts with `VDFS1002` at offset 1024; the VDFS4 path is
  unchanged. `src/vdfs1002/cli.rs` holds its CLI handler (`run`). It restores a
  tree (`restore`): the catalog walk (inode 3),
  forks, overflow extents (inode 5), Zip/Gzp descriptors, and hardlink and
  back-reference handling. Record modes are applied (`format::parse_mode`,
  field at `key_length + 32`; folder modes after all files are written).
  Symlink records (type bits `0o120000`) are created as symlinks, with the body
  as the target; links are made after folders exist and are never chmod-ed.
  It also provides the read-only modes `--list`
  (`--json`), `--audit`, and `--extract NAME -o FILE` (`select_file`, `audit`).
  `--include`, `--manifest-dir`, and `--aes-key` are rejected in VDFS1002 mode.
  The restore output directory must not exist. A bad extended-record CRC only
  warns.
  - Layout facts are documented in the module comments and tests. The
    overflow-extent record layout (inode 5) is not yet verified against a real
    image (see TODO.md). Lzo and Zlib/Gzip are decoded; Zst is rejected.
  - Tests are synthetic: `build_image` in the `tests` module writes a complete
    image in memory, and the tests cover bad magic and checksums, loops,
    truncation, overflow, zlib/gzip/plain bodies, audit, selection, and tree
    restore.
- `src/vdfs4/manifest.rs`: reserved aliases for non-portable names,
  `~vdfs-unpack-OBJECTID-SHA256`.
- `src/vdfs4/unpacker.rs`: path selection (`--include`) and safe output names.
- Other `src/vdfs4/` files: `disk` (on-disk structs), `superblock`, `btree`,
  `cattree`, `extents`, `snapshot`, `reader` (image access).

## Test fixtures

Tests build synthetic blobs in memory (`fixture`, `hashed_fixture`,
layout-5 helpers). No real Samsung images are used or stored in the repo.

`tests/fixtures/small-zlib.vdfs` (148 KB) is a `mkfs.vdfs`-built VDFS2007
image: zlib-compressed, read-only, unsigned. It holds `hello.txt`, an 8 KB
`zeros.bin`, `sub/deeper/n.txt`, and a symlink `link.txt -> hello.txt`.
`tests/fixture_zlib.rs` runs the built binary against it and checks the
SHA-256 (`9c2dc702…1f`), the file contents, the symlink target, and the exact
sets of regular files and directories.
`tests/fixture_encrypted.rs` checks `small-zlib-aes.vdfs` (SHA-256
`f71096dd…bb`).

`tests/fixtures/hashed-{md5-zlib,sha1-lzo,sha256-lzo}.vdfs` (205 KB each) are
`mkfs.vdfs` images built with `--hash-priv-rsa-key <key> --sign-all=1 -h <alg>`,
one per hash letter: `I` (MD5, Zip), `H` (SHA-1, Lzo) and `h` (SHA-256, Lzo).
They hold `hello.txt`, an 8 KB `zeros.bin`, and a 60 KB `rand.bin` (the LCG
bytes from `src/lzo1x.rs`, seed 1). `tests/fixture_hashed.rs` restores each one
and compares the three files byte for byte. The RSA key is random and was not
committed, so the images are not reproducible and their hashes are not pinned.
Regenerate them with `mkfs.vdfs` built as described below (the `RAND_bytes`
shim is not needed, because the RSA key is random anyway), and keep the
file contents and the `hashed-` names in sync with the test.

`tests/fixtures/codec-{lzo,zst}.vdfs` (205 KB each) are `mkfs.vdfs` images
built with `-c lzo` and `-c zstd` (VDFS4 layout 6, unhashed, unsigned) from the
same tree as the hashed fixtures, with the 60 KB `rand.bin`. Each holds a
descriptor tagged `Lzo` or `Zst`.

`tests/fixtures/codec-zhw.vdfs` (288 KB) is built by the upstream `vdfs-tools`
0010 `mkfs.vdfs` with a `-q` list containing `COMPRESS=zhw /zeros.bin`. Only
`zeros.bin` is compressed, because random bytes stay uncompressed under
`COMPRESS_RATIO`. It is the one codec fixture that comes from upstream output
rather than the `hsand` fork.

`tests/fixture_codecs.rs` checks each tag is present and restores all four files
byte for byte. These are synthetic: they show the codec path works, not that a
real Samsung image uses it. Regenerate them with
`MKFS=/path/to/hsand/mkfs.vdfs ZHW_MKFS=/path/to/upstream/mkfs.vdfs sh tests/fixtures/regenerate-codec.sh`.

Build the upstream `mkfs.vdfs` with
`sh tests/fixtures/build-upstream-mkfs.sh /path/to/vdfs4-tools.0010.tar.gz OUT`.
The bundled LZO 2.06 configure check and the bundled OpenSSL 1.0.1g build both
fail on gcc 15. The script compiles `lib/` and `mkfs/` by hand, with
`-fgnu89-inline` (the code relies on gnu89 `inline`) and `-no-pie`. It uses the
Debian `liblzo2-dev` and `libssl-dev` headers (fetched with `apt-get download`)
and the `libcrypto.a` shipped in the tarball. The LZO link is not exercised by
the `Zhw` fixture, which uses only zlib.

`tests/fixtures/lzo1x-long-distance.lzo` (20 KB) is a raw liblzo2 LZO1X stream
of 22000 bytes. It holds 20000 pseudo-random bytes followed by a copy of the
first 2000 bytes, which is a match 20000 bytes back (above 16 KiB, so it uses
the M4 opcode). `src/lzo1x.rs` decodes it and compares it with the same input,
rebuilt from the LCG in the test. It was produced with `lzo1x_1_compress`
(liblzo2 2.x) through `ctypes`. The 2000-byte copy compresses to about 96 bytes
where literals would cost 2000.

To rebuild them, build `mkfs.vdfs` from `hsand/vdfs4-linux` and run
`MKFS=/path/to/mkfs.vdfs sh tests/fixtures/regenerate.sh`. Build `mkfs` in
`vdfs-tools/` with `CFLAGS="-no-pie -fno-pie" make mkfs` (the bundled OpenSSL
is not PIE-compatible). Do not use `make mkfs CFLAGS+=...` on the command line:
that overrides the Makefile's `-I./include` and the build fails on `bitmap.h`. Then relink it with the random-number shim, so AES
nonces are fixed:

```sh
cc -c tests/fixtures/rand-fixed.c -o rand-fixed.o   # run from this repo
# In vdfs-tools/: rerun the link line from `make -n mkfs CFLAGS+=" -no-pie -fno-pie"`
# with `-no-pie -Wl,--wrap=RAND_bytes /path/to/rand-fixed.o` added, and output mkfs-det.vdfs.
MKFS=/path/to/vdfs-tools/mkfs-det.vdfs sh tests/fixtures/regenerate.sh
```

The script produces byte-identical images for a given `mkfs` build. It runs
`mkfs` under `faketime -f '@2020-01-01 00:00:00 x0'` (frozen clock) with
`tests/fixtures/fixed-ctime.c` preloaded and under `setarch -R` (ASLR off).
The shim is needed because `mkfs` stores each source file's ctime, and
libfaketime does not fake the `fstat`/`lstat64` calls `mkfs` makes. ASLR off
prevents uninitialized heap pointers from leaking into inode records. The
script uses a fixed build directory and relative paths, because `mkfs` embeds
its command line in the image. If you change the `mkfs` source or compiler, the
hashes change. Update them in `tests/fixture_zlib.rs` and
`tests/fixture_encrypted.rs`, then here, after regenerating.

`tests/fixtures/small-zlib-aes.vdfs` is the same tree encrypted with AES-128-CTR
(`mkfs --aes-key key.bin --encrypt-all -j 1`). The 16-byte key is the synthetic
string `0123456789abcdef`, written by the script and shared with
`tests/fixture_encrypted.rs`. It is not a secret. `-j 1` keeps chunks in table
order (multithreaded compression writes them in completion order). The tests
check the SHA-256 (`97b284a1…68fb`), the decrypted contents, the missing-key
error, and a wrong key.

Decryption is in `src/vdfs4/decompress.rs` (`decrypt_chunk`). A unit test checks it
against OpenSSL's AES-CTR output. Layout 2006 images reject the encrypted flag.
Signature trailers are still not verified (see TODO.md).

## Consumers

This crate is independent and has no dependencies on other projects. Consumers
that build it from a pinned commit and check the binary hash must re-pin after
any change under `src/`, because that changes the binary hash.
