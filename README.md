# vdfs-unpack

A Rust utility for unpacking **Samsung VDFS** filesystem images (`.img`) into a
regular host directory. It supports VDFS4 with layout version **2006** or
**2007**, and VDFS1002 (for example the HawkM `platform.img` and
`data_backup.img`). The format is detected from the image. It decompresses
compressed files, reconstructs symbolic links (e.g. `bin -> usr/bin`), and can
extract only selected paths or write a metadata manifest alongside the files
(VDFS4).

Compressed files are verified when they carry digests. The descriptor magic's
first byte selects the algorithm for layout 2007 files: `C` (none), `I` (MD5),
`H` (SHA-1), or `h` (SHA-256). Per-chunk and descriptor digests must match, and
a mismatch fails the file rather than writing unverified data. Layout-2006
chunk hashes are checked, but its descriptor hash and signature are not. The
MD5/SHA-1 paths are covered by synthetic tests only; they have not been
confirmed against a real image.

# Current project status

Implementation status of VDFS types (checked = fully implemented and tested):

| Done | Type | Gap |
|------|------|-----|
| [x] | VDFS4 layout 5 (`VDFS2006`) | |
| [x] | VDFS4 layout 6 (`VDFS2007`) | |
| [x] | VDFS4 descriptor `C` (plain) | |
| [ ] | VDFS4 descriptor `h` (SHA-256) | Layout 6: chunk and descriptor digests verified on real images (e.g. KTM2D, 5118 `h` descriptors, extracted with exit 0); layout 5: chunk digests only; signature not verified (no vendor key); public kernel has no `h` verifier |
| [ ] | VDFS4 descriptor `I` (MD5) | Restored from `mkfs.vdfs` output (`--sign-all=1 -h md5`); no real image; signature not verified |
| [ ] | VDFS4 descriptor `H` (SHA-1) | Restored from `mkfs.vdfs` output (`--sign-all=1 -h sha1`); no real image; signature not verified |
| [ ] | VDFS4 descriptor `F` (forced) | Rejected by `DigestAlgo::from_magic` |
| [x] | VDFS4 codec `Zip` (zlib) | |
| [x] | VDFS4 codec `Gzp` (gzip) | |
| [x] | VDFS4 codec `Lzo` | Verified against liblzo2 reference streams and a synthetic `mkfs.vdfs` image (`codec-lzo.vdfs`); no valid layout 5/6 `Lzo` descriptor in the 13 cached VDFS4 images |
| [x] | VDFS4 codec `Zst` (zstd) | Synthetic `mkfs.vdfs` image (`codec-zst.vdfs`) only; no valid layout 5/6 `Zst` descriptor in the cached VDFS4 images |
| [x] | VDFS4 codec `Zhw` (zlib, 16-byte aligned chunks) | Verified against upstream `vdfs-tools` 0010 `mkfs.vdfs` output (`codec-zhw.vdfs`, decoded as zlib in `src/vdfs4/decompress.rs`); no valid layout 5/6 `Zhw` descriptor in the cached VDFS4 images |
| [x] | VDFS1002 superblock and detection | |
| [x] | VDFS1002 record 1 (folder) | |
| [x] | VDFS1002 record 2 (file) | |
| [x] | VDFS1002 record 3 (hard link) | |
| [x] | VDFS1002 record 5 (indirect link, back-reference) | |
| [x] | VDFS1002 codecs `Zip` and `Gzp` | Descriptor marker `C` or `I`; the chunk table is found by the descriptor CRC, with up to 64 KiB padding before the descriptor. Real HawkM image restores with 0 skipped |
| [ ] | VDFS1002 codec `Zst` | Rejected by `vdfs1002/codec.rs` (test `rejects_zstd_compressed_file` in `vdfs1002/tests.rs`) |
| [x] | VDFS1002 codec `Lzo` | Verified against liblzo2 reference streams only; no real image has used LZO |
| [ ] | VDFS1002 overflow extents (inode 5) | Layout not confirmed on a real image |

## Build and usage

```sh
cargo build --release --locked
./target/release/vdfs-unpack <image.img> -o <output_dir>
```

Arguments:

- `<INPUT>` — path to the VDFS image (e.g. `platform.img`).
- `-o, --output <OUTPUT>` — target directory for extraction. Defaults to
  `<image-stem>-extracted` in the current directory.
- `--include <PATH>` — extract only this path (repeatable, up to 1024 values).
  Paths are image paths such as `root/usr/bin/SWUCoreTV`. Without `--include`
  the whole image is extracted.
- `--aes-key <FILE>` — 16 raw bytes of AES-128 key for encrypted chunks
  (`ENCRYPTED` flag, layout 2007). Required when the image has encrypted chunks;
  the key is not validated, so a wrong key fails with a decompression or digest
  error.
- `--force` — extract with the VDFS4 path even when the image is not a
  recognized VDFS1002 or VDFS4 image. Without it, such an image is rejected.
- `--manifest-dir <DIR>` — write these files into `DIR` (must not already
  contain them):
  - `catalog.bin`: raw catalog records.
  - `entries.tsv`: tab-separated entry list.
  - `xattrs.bin`: extended attributes, with raw values in the `OSEAX01` format.

Manifest mode extracts the same files, directories and symbolic links as plain
extraction, with the same names. It additionally records every entry in the
manifest. Differences:

- Symbolic link targets longer than 65536 bytes are rejected.
- Device, FIFO and socket nodes are recorded as `NODE` rows only and nothing is
  written to disk. Plain extraction writes an empty regular file in their place.
- Limits: 64 MiB per table or catalog, and 200,000 catalog entries.

Manifests contain raw extended-attribute values, so review them before sharing.

## VDFS1002 images

`platform.img` and `data_backup.img` (HawkM) use the VDFS1002 format, detected by
the `VDFS1002` magic at offset 1024. For these images the default action is a
restore into `-o`. The read-only modes are:

- `--list [--json] [--parent <ID>]` — print catalog records as
  `parent_id<TAB>object_id<TAB>type<TAB>name` (or a JSON document).
- `--audit` — print block-reach and file-fork consistency as JSON.
- `--extract <NAME> [--parent <ID>] -o <FILE>` — write one file record to a new
  file and print its size and SHA-256.

`--list`, `--audit` and `--extract` are mutually exclusive.

## Testing

```sh
cargo test --locked
```

GitHub Actions runs this command on every pull request targeting `master`
(see `.github/workflows/test.yml`).

## License

This project is distributed under the Apache License, Version 2.0. See the
[LICENSE](LICENSE) file.

## Related resources

- https://github.com/synacktiv/samsung-q60t-exploit
- https://github.com/bugficks/msddecrypt
- https://github.com/hsand/vdfs4-linux
- https://github.com/HinTak/vdfs-tools
- https://github.com/HinTak/vdfs-driver
- https://github.com/theubusu/unixtract
- https://github.com/gtors/vdfs-unpack
- https://sourceforge.net/projects/vdfs/
