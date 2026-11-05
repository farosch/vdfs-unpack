//! LZO1X block decoding for compressed VDFS4 and VDFS1002 chunks.
//!
//! A chunk is one raw LZO1X stream that ends with the end-of-stream marker.
//! The caller passes the largest output it accepts, so a corrupt or
//! oversized block fails instead of growing without bound.

use anyhow::{Result, anyhow};

pub fn decompress(input: &[u8], max_out: usize) -> Result<Vec<u8>> {
    ::lzo::decompress(input, max_out).map_err(|err| anyhow!("LZO1X decompression failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::decompress;
    use crate::hex::decode as unhex;

    // Reference streams from liblzo2 (lzo1x_1_compress).
    const HELLO: &str = "1668656c6c6f110000";
    const EMPTY: &str = "110000";
    const TEXT: &str = "085644465320626c6f636b202000ec28000c6f636b205644465320626c6f636b20110000";
    const PATTERN_251: &str = "00f300070e151c232a31383f464d545b626970777e858c939aa1a8afb6bdc4cbd2d9e0e7eef501080f161d242b323940474e555c636a71787f868d949ba2a9b0b7bec5ccd3dae1e8eff6020910171e252c333a41484f565d646b727980878e959ca3aab1b8bfc6cdd4dbe2e9f0f7030a11181f262d343b424950575e656c737a81888f969da4abb2b9c0c7ced5dce3eaf1f8040b121920272e353c434a51585f666d747b828990979ea5acb3bac1c8cfd6dde4ebf2f9050c131a21282f363d444b525960676e757c838a91989fa6adb4bbc2c9d0d7dee5ecf3fa060d141b222930373e454c535a61686f767d848b9299a0a7aeb5bcc3cad1d8dfe6edf400070e151c232a31383f200000000000000000000000000000d9e8030cccd3dae1e8eff6020910171e252c33110000";

    fn pattern_251() -> Vec<u8> {
        (0..4096).map(|i| ((i * 7) % 251) as u8).collect()
    }

    #[test]
    fn decodes_reference_streams() {
        assert_eq!(decompress(&unhex(HELLO), 5).unwrap(), b"hello");
        assert_eq!(decompress(&unhex(EMPTY), 0).unwrap(), b"");
        assert_eq!(
            decompress(&unhex(TEXT), 550).unwrap(),
            b"VDFS block ".repeat(50)
        );
        assert_eq!(
            decompress(&unhex(PATTERN_251), 4096).unwrap(),
            pattern_251()
        );
    }

    /// Input for `tests/fixtures/lzo1x-long-distance.lzo`: 20000 LCG bytes
    /// (seed 1, `x = x * 1103515245 + 12345 mod 2^31`, byte = `x >> 16`), then
    /// a copy of the first 2000 bytes. The copy is 20000 bytes back, which
    /// needs the LZO1X M4 opcode (distances above 16 KiB).
    fn long_distance_input() -> Vec<u8> {
        let mut x: u64 = 1;
        let mut head: Vec<u8> = (0..20000)
            .map(|_| {
                x = (x * 1103515245 + 12345) & 0x7fff_ffff;
                (x >> 16) as u8
            })
            .collect();
        let copy = head[..2000].to_vec();
        head.extend_from_slice(&copy);
        head
    }

    #[test]
    fn decodes_long_distance_match() {
        let stream = include_bytes!("../tests/fixtures/lzo1x-long-distance.lzo");
        let expected = long_distance_input();
        assert_eq!(expected.len(), 22000);
        assert_eq!(decompress(stream, 22000).unwrap(), expected);
        assert!(decompress(stream, 21999).is_err());
    }

    #[test]
    fn output_bound_is_an_upper_limit() {
        assert_eq!(decompress(&unhex(HELLO), 100).unwrap(), b"hello");
        assert!(decompress(&unhex(HELLO), 4).is_err());
        assert!(decompress(&unhex(PATTERN_251), 4095).is_err());
    }

    #[test]
    fn rejects_truncated_missing_eos_and_trailing_input() {
        let hello = unhex(HELLO);
        assert!(decompress(&hello[..hello.len() - 1], 5).is_err());
        assert!(decompress(&hello[..hello.len() - 3], 5).is_err());
        let mut trailing = hello.clone();
        trailing.push(0);
        assert!(decompress(&trailing, 5).is_err());
    }
}
