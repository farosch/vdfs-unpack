#!/bin/sh
# Builds the upstream vdfs-tools mkfs.vdfs (vdfs4-tools 0010, HinTak/vdfs-tools)
# on a current toolchain. The bundled LZO 2.06 configure check and the bundled
# OpenSSL 1.0.1g build both fail on gcc 15, so this compiles lib/ and mkfs/ by
# hand against the Debian liblzo2 headers and the libcrypto.a shipped in the
# tarball. The Zhw codec is zlib-only, so the LZO link is not exercised.
# Usage: sh tests/fixtures/build-upstream-mkfs.sh TARBALL OUTPUT
# Needs apt-get (download only, no root) and dpkg-deb.
set -eu

TARBALL=${1:?usage: build-upstream-mkfs.sh TARBALL OUTPUT}
OUTPUT=${2:?usage: build-upstream-mkfs.sh TARBALL OUTPUT}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/vdfs-upstream-mkfs.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

tar xzf "$TARBALL" -C "$WORK"
SRC=$WORK/vdfs-tools
mkdir -p "$WORK/deb" "$WORK/ossl"
(cd "$WORK/deb" && apt-get download liblzo2-dev libssl-dev >/dev/null 2>&1)
for deb in "$WORK"/deb/*.deb; do
	dpkg-deb -x "$deb" "$WORK/pkgroot"
done
tar xzf "$SRC/openssl/openssl-1.0.1g.tar.gz" -C "$WORK/ossl"
OSSL=$WORK/ossl/openssl-1.0.1g
ROOT=$WORK/pkgroot
LZO_SO=/usr/lib/x86_64-linux-gnu/liblzo2.so.2

CFLAGS_UPSTREAM="-std=gnu99 -fgnu89-inline -fcommon -fno-pie -O0 -w \
-D_FILE_OFFSET_BITS=64 -DUSER_SPACE -DCONFIG_VDFS4_DEBUG -DCOMPRESS_RATIO=\"75\" \
-I$SRC/include -I$OSSL/include -I$ROOT/usr/include -I$ROOT/usr/include/lzo \
-include sys/sysmacros.h"

cd "$SRC"
for f in lib/*.c mkfs/*.c; do
	# shellcheck disable=SC2086
	cc $CFLAGS_UPSTREAM -c "$f" -o "${f%.c}.o"
done
# shellcheck disable=SC2086
cc -no-pie -std=gnu99 -fgnu89-inline -o mkfs.vdfs lib/*.o mkfs/*.o \
	-lrt -lpthread -lz "$LZO_SO" "$OSSL/libcrypto.a" -ldl
cp mkfs.vdfs "$OUTPUT"
echo "built $OUTPUT"
