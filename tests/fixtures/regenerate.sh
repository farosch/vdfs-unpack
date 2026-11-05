#!/bin/sh
# Rebuilds small-zlib.vdfs and small-zlib-aes.vdfs with mkfs.vdfs from
# hsand/vdfs4-linux.
# Usage: MKFS=/path/to/mkfs.vdfs sh tests/fixtures/regenerate.sh
# MKFS must be linked with rand-fixed.c (see DEVELOP.md), so that the random
# AES nonces are fixed. mkfs runs with a frozen libfaketime clock (x0),
# fixed-ctime.c preloaded, and ASLR disabled (setarch -R), because uninitialized
# heap pointers otherwise leak into inode records. Output is byte-identical for
# a given mkfs build. The SHA-256 values in tests/fixture_zlib.rs and
# tests/fixture_encrypted.rs must match. Changing the mkfs source or compiler
# version changes the hashes.
set -eu

MKFS=${MKFS:?set MKFS to the mkfs.vdfs binary built from hsand/vdfs4-linux}
HERE=$(cd "$(dirname "$0")" && pwd)
# mkfs stores its command line (including paths) in the image, so the build
# directory must be fixed and paths relative to it.
WORK=${TMPDIR:-/tmp}/vdfs-fixture-build
rm -rf "$WORK"
mkdir -p "$WORK/src/sub/deeper"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

printf 'hello from the vdfs fixture\n' > src/hello.txt
printf 'nested\n' > src/sub/deeper/n.txt
head -c 8192 /dev/zero > src/zeros.bin
ln -s hello.txt src/link.txt
find src -exec touch -h -d @0 {} +

cc -shared -fPIC -O2 -o "$WORK/fixed-ctime.so" "$HERE/fixed-ctime.c"
# mkfs also embeds argv[0] (its invocation path) in the image. The committed
# fixture was built at this exact path, so the script runs mkfs from it.
MKFS_PATH=/tmp/vdfs-scratch/vdfs4-linux/vdfs-tools/mkfs.vdfs
mkdir -p "$(dirname "$MKFS_PATH")"
cp "$MKFS" "$MKFS_PATH"
trap 'rm -rf "$WORK" "$MKFS_PATH"' EXIT
FAKETIME='@2020-01-01 00:00:00 x0'

LD_PRELOAD="$WORK/fixed-ctime.so" faketime -f "$FAKETIME" setarch -R "$MKFS_PATH" out.vdfs -i -r src -z 4M -c zlib --read-only -t 0
cp out.vdfs "$HERE/small-zlib.vdfs"
sha256sum "$HERE/small-zlib.vdfs"

# Synthetic test key. It is not secret and is only used by the fixture tests.
# Keep it in sync with TEST_KEY in tests/fixture_encrypted.rs.
printf '0123456789abcdef' > key.bin
# -j 1 keeps chunk order deterministic (multithreaded compression writes chunks
# in completion order). The output name must match the one used for the
# committed hash because mkfs embeds its command line.
rm -f out.vdfs
LD_PRELOAD="$WORK/fixed-ctime.so" faketime -f "$FAKETIME" setarch -R "$MKFS_PATH" out.vdfs -i -r src -z 4M -c zlib --read-only -t 0 \
	--aes-key key.bin --encrypt-all -j 1
cp out.vdfs "$HERE/small-zlib-aes.vdfs"
sha256sum "$HERE/small-zlib-aes.vdfs"
