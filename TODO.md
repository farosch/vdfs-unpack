## Authenticated hZip: `C` magic with a signature trailer

Upstream PR gtors/vdfs-unpack#2 reads the 0/128/256-byte signature trailer for
any descriptor magic. This crate reads it only for hashed (`I`/`H`/`h`) descriptors. For `C`
descriptors it reads no trailer and ignores `sign_type`.

Kernel evidence (`hsand/vdfs4-linux`, `inode.c:4029-4060`): the signature
trailer size is added only for `I`/`H`/`h`. Verification runs only when
`hash_fn` is set, which requires a hashed magic (`inode.c:4145`). So ignoring
the trailer for `C` matches the kernel. This lead is resolved as far as the
kernel goes.

Answer: yes, real VDFS2006 images contain `h` (SHA-256) descriptors.

- Sample: `platform.img` from `T-JZMDEUC` (2016 JazzM, unpacked with
  unixtract). Its superblock is `VDFS2006`, and the embedded mkfs command line
  includes `--sha256 -H`.
- Layout-5 descriptor tally (prefix, codec, count): `C` Gzp 7782, `h` Gzp 3291.
  No `H` (SHA-1) descriptors. The single `I` byte match is a false positive:
  the bytes after it are ASCII text, not a layout-5 tail.
- Our extractor reads the whole 1.3 GB image with exit 0 and 52,052 files. Every
  `h` chunk SHA-256 is checked (`decompress_layout5`, `src/vdfs4/decompress.rs`) and none mismatch. The
  descriptor digest and signature are still not verified (no vendor key).
- Layout-6 `h` descriptor digest (KTM2D, `upgrade.msd.plain/sections/8.bin`,
  5118 `h` descriptors): `decompress_blob` checks it (`src/vdfs4/decompress.rs`,
  around lines 492–496). Extraction exits 0 with no mismatch, so the check
  passes on real data. Layout 5 still has no descriptor-digest check.
- Survey of 8 real images (the JazzM sample plus 7 more with public MSD11 keys:
  KTM2D, NKM2, NKM, MSM, PTMC, PTMD, HKMF). Seven are VDFS2007 (layout 6), with
  `h` (SHA-256) descriptors in every one, for example 5118 in KTM2D and 10178
  in NKM2. Across all eight, no `I` (MD5) or `H` (SHA-1) descriptor appears.
  The `I` and `H` mapping is still covered only by synthetic fixtures and PR #2.
- Signature check: the firmware zips' metadata (`REL.xml`, `USBHardwareKeys.xml`,
  `info.txt`) contains no public key or certificate. `REL.xml` names
  `RSAPublicProductKey` and `itemsPrivateRSAKey.txt` only as build-server
  paths. Partitions from the JazzM image (`secos.bin`, `secos_drv.bin`,
  `seret.bin`, `sign.bin`, `uImage`, `dtb.bin`, `ddr.init`) contain no standard
  RSA key structure (PEM, SPKI, PKCS#1, X.509) and no `65537` exponent next to a
  modulus. Trailers stay unverified. Static look at the secure OS: `secos.bin`
  (1040384 bytes) has no recognised container magic and entropy 7.33 bits/byte,
  so it is encrypted or packed. `secos_drv.bin` and `seret.bin` are not the
  verifier: `seret.bin` is the Linux boot stub. Reaching the verification code
  would mean breaking the secure OS's encryption, which is out of scope, so
  that route is closed. Trailers stay unverified.
  The lead stays open until a real image with `I` or `H` is found, or until we
  decide the unobserved paths are acceptable as synthetic-only.
- Public Samsung kernel source (`doitaljosh/linux-3.10.30-famhubv1`, the tree
  matching JazzM's uImage `Linux 3.10.30`), `fs/vdfs/`:
  - `vdfs_layout.h:102-104,127`: descriptor starts are `C` (plain), `H`
    (authenticated) and `F` (forced). `VDFS_HASH_LEN` is 20 (SHA-1).
  - `inode.c:3636-3637`: any other first magic byte, including `h` and `I`,
    returns `-EOPNOTSUPP`.
  - `inode.c:3559-3580`: the RSA check hashes `(extents+1)*20` bytes and checks
    the 256-byte signature after them, using `vdfs_verify_rsa_sha1_signature`
    (`super.c:755-772`) and the compiled-in key (`Secureboot.c:159-199`).
  - Consequence: this public kernel has no path that verifies an `h` (SHA-256)
    descriptor, and JazzM's 3291 `h` descriptors would not open under it. Either
    the device kernel differs from the public tree, or `h` is not the SHA-256
    format our parser assumes. Our parser's `h` handling is therefore a
    hypothesis, and the chunk-digest check passing is evidence only that
    the SHA-256 table is self-consistent.
  - `F` is not handled by our parser (`DigestAlgo::from_magic` rejects it). No
    real image has been checked for `F`.
  - Public keys: `publicKey.h` holds 10 RSA-2048 moduli (e=65537) for the
    SDP1304/1406/1404/1412 variants. None is for SDP1501, which JazzM's
    `dtb.bin` names. Checking the JazzM superblock against all 10 gave 0 hits.
    That superblock's `sb_hash` region is zero, so the release image carries
    no superblock signature to test.
  - Status: the kernel-side verifier is public but applies only to `H`/`F`
    files, which no sampled image contains. The lead is open because the
    device-kernel `h` behaviour is unknown.
  - `F` scan (12 decrypted firmwares, all `platform.img`/`rocommon.img`/`secos.bin`
    under `image/out`): no `F` descriptor exists. Every `F`+`Zip|Gzp|Lzo|Zst` hit
    is random bytes inside compressed data, with a garbage layout field at +6.
    Valid layout-5/6 hits are `C` and `h` only. Same result for `H` and `I`
    (`I` appears in HKMF, which is VDFS1002 and not parsed). Images with no
    matching key (KSU2E, MSL, PTML) were not extracted, so they are not covered.
- Local `T-HKMFDEUC` `section-6.bin` and `data_backup.img` report layout `1002`
  and are rejected as unsupported. They are not VDFS2006/2007.
- `T-KSU2EDEUC` `upgrade.msd` could not be decrypted. Its key is not in
  unixtract's public list (only `KantSU2E 2023` for `T-KSU2EC*`). The 13 key
  files in the local key directory were compared by value, not name: each
  matches a public list entry, and none is the KSU2E key. They cannot decrypt
  this image.
- Searched the local firmware sections for VDFS2006/2007 magic. The only hits
  are our own `mkfs` fixtures. The firmware sections are VDFS1002 or unknown.
- Resolved: the firmware's `usr/bin/mkfs.vdfs` is a VDFS1002 `IGzp` file. Its
  descriptor starts with `I`, not `C`, so the codec returned the raw gzip
  members. The chunk table also sits 320 bytes before the descriptor. Both are
  now handled (see the VDFS1002 section). The restored file is an ARM EXEC ELF
  with interpreter `/lib/ld-linux.so.3`. The earlier "128 KB non-ELF prefix" was
  an artefact of decoding the concatenated members as one gzip stream.
- Resolved (VDFS1002 restore): record modes are read from the fixed record
  field at `key_length + 32` (folders and files), and symlink records (type
  bits `0o120000`) are restored as real symlinks instead of files holding the
  target text. Verified on `section-6.bin`: `bin/sh -> bash` and
  `lib/ld-linux.so.3 -> ld-2.18.so`, and `mkfs.vdfs` runs under `qemu-arm`.
- Partial answer: `mkfs.vdfs` from `hsand/vdfs4-linux` writes `VDFS2007`
  images. Unsigned, uncompressed and zlib read-only images round-trip byte for
  byte in manifest mode.
- `mkfs.vdfs -s <key>` changes only the superblock signature. Per-file hashing
  needs `--hash-priv-rsa-key <key> --sign-all=1 -h {md5,sha1,sha256}` (`is_need_sign`
  only signs executables otherwise). `--sign-all` takes a required argument, so
  write it as `--sign-all=1`, or it swallows the next option. With that
  combination `mkfs` writes `IZip`, `HLzo` and `hLzo` descriptors. The
  `hashed-*.vdfs` fixtures (see `DEVELOP.md`) restore byte-exact, so the hashed
  decode paths now have real-tool coverage. The lead on whether device images use
  `I`/`H` stays open.
- With `C` descriptors, a single corrupted data byte is accepted and changes
  the extracted output silently (no digest exists to check). Expected for
  unhashed data, but it means integrity is not checked for these files.

## VDFS4 `Zhw` codec (verified against upstream mkfs output)

Upstream `vdfs-tools` 0010 (`include/vdfs4_layout.h:88`, `lib/decompress.c:133-135`,
`353`) defines a `Zhw` descriptor magic. It decodes with the zlib decompressor and
pads hardware chunks to 16-byte alignment (`lib/compress.c:231-237`). The zlib
decoder stops at the end of its stream, so the padding is ignored.

Implemented as a zlib alias in `src/vdfs4/decompress.rs`, test
`decodes_zhw_chunk_with_16_byte_padding`. `tests/fixtures/codec-zhw.vdfs` is
real upstream `mkfs.vdfs` output (`-q` list with `COMPRESS=zhw /zeros.bin`),
built by `tests/fixtures/build-upstream-mkfs.sh`. `tests/fixture_codecs.rs`
restores it byte for byte.

- Scan: the 13 cached `*.plain.zip` VDFS4 images contain raw `Zhw` byte matches,
  but none has a valid layout 5/6 value at +6. No real image is known to use
  `Zhw`, and the matches are noise.
- Open: VDFS1002 does not accept `Zhw` (its codec list is separate).



## `ILINK` records become empty directories without manifest mode (resolved)

`ILINK` (type 0x05) rows are reverse-parent index entries, not listed
children. The kernel's `readdir` skips them (`inode.c:195`), while `lookup`
and `__vdfs4_iget` follow them. `mkfs` writes one per object whose
`object_id != parent_id` (`vdfs-tools/lib/cattree.c:345-352`). Our walk used
to treat them as children and created spurious `a/a` directories. Now
`process_directory` skips them in both modes and records them only in
manifest mode. `extracted_directories_are_exactly_the_expected_set` covers this.

## Symlinks in non-manifest mode (resolved)

Non-manifest mode now creates symlinks. Link targets are collected during the
walk and created only after every regular file is written, so no write can
follow a link planted by the image. An existing file at the link path is
replaced; an existing directory is an error. Unit tests cover the replace and
refuse cases. `small-zlib.vdfs` contains `link.txt -> hello.txt`, and
`symlink_is_created_with_recorded_target` checks it end to end.

## Reconciling with upstream PR #2 (resolved)

- PR #1 (btree refactor) does not compile against our callers. Not taken.
- PR #2 (authenticated hZip) drops the single-stream fallback and does not
  verify chunk or descriptor digests. Not taken as written.
- Taken from PR #2: the `I` (MD5) and `H` (SHA-1) digest mapping, added in
  `c378669`. Covered by `md5_and_sha1_hashed_chunks_verify` and
  `md5_and_sha1_reject_corrupt_hashes`.
- Kept: digest verification and the single-stream fallback.
- Still open: whether real images use hashed magics (see the first lead).

## Encryption (resolved, decryption only)

Implemented: AES-128-CTR chunk decryption for layout 2007 when the
`ENCRYPTED` (0x2) chunk flag is set. Pass the 16-byte raw key with `--aes-key`.
Each chunk is decrypted before its digest is checked and before decompression.
The IV is `nonce[8] || LE64(chunk_index * chunk_size / 16)`, where
`chunk_index` is the table position. The nonce comes from the descriptor
(`aes_nonce`). Layout 2006 rejects the flag.

Tests: `tests/fixture_encrypted.rs` (the synthetic image, correct and wrong keys,
no key), and the `decrypt_chunk_matches_aes_ctr_reference` unit test.

Not covered: signature trailers are still not verified, and no vendor keys
exist to test against. Keys in the repo are synthetic test values only.

## `mkfs.vdfs` output reproducibility (resolved)

Earlier runs differed by about 48 bytes. The causes were:

- Superblock timestamp (`0x214`/`0x218`): real clock. Fixed by freezing the
  clock with `faketime -f '@... x0'`.
- Per-file ctime (`st_ctim` in catalog records): real time, because
  libfaketime does not fake the `fstat`/`lstat64` symbols `mkfs` imports.
  Fixed by `tests/fixtures/fixed-ctime.c`, which is preloaded by
  `regenerate.sh`.
- Embedded command line and paths: fixed by using a fixed build directory and
  relative paths.
- Uninitialized heap pointers in inode records (ASLR-dependent): fixed with
  `setarch -R`.
- AES nonces (`RAND_bytes`): fixed by linking `mkfs` with
  `-Wl,--wrap=RAND_bytes` against `tests/fixtures/rand-fixed.c`.
- Multithreaded chunk order in encrypted images: fixed with `-j 1`.

Both images are byte-identical across runs (checked by regenerating twice).

## VDFS1002 support (`src/vdfs1002/`): open leads

The HawkM `platform.img` and `data_backup.img` use VDFS1002. The Rust reader
is an independent implementation. Its synthetic tests cover the format rules.
Real-image checks are listed below.

- Overflow extent layout (inode 5): the 24-byte keys and 40-byte records
  (object at +8, iblock at +16, begin at +24, length at +32) follow the Python
  reader, but they are not confirmed on a real image. The HawkM `platform.img`
  has 0 overflow objects, so this is still open. The kernel driver
  `HinTak/vdfs-driver` (`vdfs4_layout.h`, `extents.c`) agrees with the VDFS4
  record: an 8-byte generic key (`magic[4]`, `key_len`, `record_len`), then
  object id at +8, iblock at +16, extent begin at +24 and length at +32, for a
  40-byte record. That confirms the VDFS4 layout only. It has no VDFS1002
  source, so the VDFS1002 inode 5 lead stays open. `extents_resolve_fork` also
  filters overflow records with `iblock >= inline_end`, a heuristic the kernel
  does not use. Check it on a VDFS4 image that has overflow records.
- Real image: the HawkM MSDU10 key is the 16-byte `HawkM` entry in the `MSD10`
  collection of unixtract's public `src/keys.ukf`. unixtract extracts
  `image/upgrade.msd` to `platform.img`, and `vdfs-unpack` restores it. Result:
  20,280 directories, 46,535 files, 2,093 hard-link names, 66,814
  back-references, and 981 nameless file bodies (exit 0). Those 981 are not
  failures. Each is a self-parented file record with an empty name, and each
  is the data body of at least one hard-link record, so they are now counted
  as "Nameless file bodies (hard-link data)" and not as skipped. The
  remaining skipped count is 0 on the HawkM image.
- `--list`, `--audit`, and `--extract` match the Python reader on the HawkM
  image: 136,703 listing rows, the same audit JSON, and identical extracted
  bytes.
- Compressed descriptors: the HawkM image has 3,213 `I`-marked descriptors
  (about 3,200 files) and 42,061 `C`-marked ones with the table directly
  before the descriptor. Before this fix, the `I` ones were silently written
  as raw gzip, and the restore still reported 0 skipped. The table gap varies
  per descriptor (0 to over 1 KB seen), so `locate_chunk_table` searches back
  up to 64 KiB for the `XT` start whose CRC matches. Re-restored with the
  fixed build: 46,535 files, 0 skipped, and no raw-gzip executables left
  (`usr/bin/mkfs.vdfs` is ELF). Remaining `1f8b` files are real `.gz` payloads.
  Survey re-run on the HawkM image (`section-6.bin`, the input whose SHA-256
  matches `restore.json`), with `survey_compressed_descriptors`: 47,516 file
  bodies (46,535 restored files plus 981 nameless bodies). 45,285 compressed
  descriptors decode with the CRC verified: 44,924 `decoded` and 361 whose
  output is itself gzip (real `.gz` payloads). 2,230 bodies are raw, with no
  marker and no codec pattern in the last 64 KiB. There are 0 read, CRC,
  descriptor, or decode errors. The table gaps take 54 distinct values, up to
  4,768 bytes, so all sit inside the 64 KiB window. The one flagged body is a
  38-byte symlink target (`libug-IPv6_StatusUG-efl.so.15.09.03.01`) whose
  text contains `IPv6`, a false positive that is restored raw. The earlier
  "9 unmatched" descriptors are decoded in this run, with no CRC mismatch.
- Lzo is decoded (LZO1X, `src/lzo1x.rs`) for VDFS4 and VDFS1002. Zst compressed
  descriptors are rejected. No real image has exercised Lzo, and the kernel's
  handling of trailing bytes after the end-of-stream marker is unverified.

## VDFS4 small-area and tiny-file storage (closed as not feasible for now)

- Upstream `vdfs-tools` documents a small area: files smaller than a chunk can
  be stored in the image small area, addressed through the extent tree, and
  mounted with the `tiny`/`tinysmall` options. `vdfs-unpack` has no small-area
  reader, so such files would be read from the wrong location or skipped.
- The `vdfs-tools` header also defines `VDFS4_CHUNK_FLAG_UNCOMPR` (handled) and
  a hardware-compression page-per-chunk constant (`VDFS4_HW_COMPR_PAGE_PER_CHUNK`
  = 32), which we do not use.
- Findings (checked against the sources, not yet acted on):
  - `vdfs-tools` 0010 declares `init_small`, `test_and_set_small_area_bitmap_bit`
    and `copy_small_file_to_image` in `include/vdfs_tools.h`, but no `small.c`
    exists in `lib/`, `mkfs/` or `unpack/`.
  - Tiny-file handling is commented out in `unpack/unpack.c` (around lines
    819 and 1388) and `mkfs/mkfs.c` (lines 419, 681, 872); `TINY_FILE` is never
    defined.
  - `HinTak/vdfs-driver` `snapshot.c` (~line 1095) says there is "no such thing
    as ... small files bitmap anymore". Only the `tiny`/`tinysmall` mount-option
    names remain (`vdfs4.h`, `vdfs.txt` lines 36-38).
  - Conclusion: no reference implements a small-area format, so a reader would
    be guesswork and cannot be verified. Blocked until a real image shows a
    tiny or small extent.
