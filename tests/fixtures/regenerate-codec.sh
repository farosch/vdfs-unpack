#!/bin/sh
# Rebuilds codec-lzo.vdfs and codec-zst.vdfs with mkfs.vdfs from
# hsand/vdfs4-linux, and codec-zhw.vdfs with the upstream vdfs-tools mkfs.vdfs
# (see build-upstream-mkfs.sh). The tree and rand.bin match
# tests/fixture_codecs.rs.
# Usage: MKFS=/path/to/hsand/mkfs.vdfs ZHW_MKFS=/path/to/upstream/mkfs.vdfs \
#        sh tests/fixtures/regenerate-codec.sh
# Build setup is the same as regenerate.sh (frozen clock, fixed ctime, ASLR
# off). The output is not pinned by a hash, because the fixture test checks
# the restored contents instead.
set -eu

MKFS=${MKFS:?set MKFS to the mkfs.vdfs binary built from hsand/vdfs4-linux}
ZHW_MKFS=${ZHW_MKFS:?set ZHW_MKFS to the upstream vdfs-tools mkfs.vdfs}
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=${TMPDIR:-/tmp}/vdfs-codec-build
rm -rf "$WORK"
mkdir -p "$WORK/src/sub/deeper" "$WORK/vdfs-tools"
trap 'rm -rf "$WORK"' EXIT
# mkfs embeds its invocation path in the image. Running it from a copy in the
# work directory keeps the input binary untouched.
MKFS_PATH=$WORK/vdfs-tools/mkfs.vdfs
cp "$MKFS" "$MKFS_PATH"
cd "$WORK"

printf 'hello from the vdfs fixture\n' > src/hello.txt
printf 'nested\n' > src/sub/deeper/n.txt
head -c 8192 /dev/zero > src/zeros.bin
# The same LCG as tests/fixture_codecs.rs, seed 1.
python3 -c '
x = 1
out = bytearray()
for _ in range(60000):
    x = (x * 1103515245 + 12345) % (1 << 31)
    out.append((x >> 16) & 0xff)
open("src/rand.bin", "wb").write(out)'
find src -exec touch -h -d @0 {} +

cc -shared -fPIC -O2 -o "$WORK/fixed-ctime.so" "$HERE/fixed-ctime.c"
FAKETIME='@2020-01-01 00:00:00 x0'
run_mkfs() {
	LD_PRELOAD="$WORK/fixed-ctime.so" faketime -f "$FAKETIME" setarch -R "$MKFS_PATH" "$@"
}

run_mkfs lzo.vdfs -i -r src -z 4M -c lzo --read-only -t 0
cp lzo.vdfs "$HERE/codec-lzo.vdfs"
sha256sum "$HERE/codec-lzo.vdfs"

run_mkfs zst.vdfs -i -r src -z 4M -c zstd --read-only -t 0
cp zst.vdfs "$HERE/codec-zst.vdfs"
sha256sum "$HERE/codec-zst.vdfs"

# Upstream mkfs has no -c option. Files are compressed through the -q list.
# zeros.bin is the only compressible file: random bytes stay uncompressed
# under COMPRESS_RATIO.
ZHW_PATH=$WORK/vdfs-tools/mkfs-upstream.vdfs
cp "$ZHW_MKFS" "$ZHW_PATH"
printf 'COMPRESS=zhw\t/zeros.bin\n' > zhw.list
LD_PRELOAD="$WORK/fixed-ctime.so" faketime -f "$FAKETIME" setarch -R \
	"$ZHW_PATH" zhw.vdfs -i -r src -q zhw.list -z 4M --read-only -t 0
cp zhw.vdfs "$HERE/codec-zhw.vdfs"
sha256sum "$HERE/codec-zhw.vdfs"
