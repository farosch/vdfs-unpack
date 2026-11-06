use super::codec::{compressed_descriptor, decompress_blob, locate_chunk_table};
use super::format::{
    COMPRESSED_CRC_OFFSET, COMPRESSED_DESCRIPTOR_SIZE, COMPRESSED_LAYOUT, COMPRESSED_TABLE_ENTRY,
    EXTENT_KEY_SIZE, EXTENT_RECORD_SIZE, Extent, FORK_SIZE, KEY_FIXED_SIZE, MAX_READ_BYTES,
    NODE_INDEX, NODE_LEAF, Overflow, RECORD_FILE, RECORD_FOLDER, RECORD_HARDLINK,
    RECORD_INDIRECT_LINK, RECORD_MODE_OFFSET, file_extents, kernel_crc32,
};
use super::*;
use crate::hex::decode as hex;
use anyhow::Result;
use flate2::Compression;
use flate2::write::{GzEncoder, ZlibEncoder};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const BLOCK: usize = 4096;
const NODE: usize = 16384;
const SUPERBLOCK: usize = 1024;
const EXTENDED: usize = 1536;
const BASE_BLOCK: usize = 6;
const NODE_EXTENT_BLOCK: usize = 16;
const HEAD_NODE_OFFSET: usize = NODE_EXTENT_BLOCK * BLOCK;
const ROOT_NODE_OFFSET: usize = (NODE_EXTENT_BLOCK + 4) * BLOCK;
const LEAF_NODE_OFFSET: usize = (NODE_EXTENT_BLOCK + 8) * BLOCK;
const EXTENT_HEAD_OFFSET: usize = (NODE_EXTENT_BLOCK + 12) * BLOCK;
const EXTENT_LEAF_OFFSET: usize = (NODE_EXTENT_BLOCK + 16) * BLOCK;
const BASE_RECORDS: usize = 256;
const EXTENT_TABLE: usize = 512;
const IMAGE_BLOCKS: usize = 40;

fn kcrc(data: &[u8]) -> u32 {
    kernel_crc32(&[data])
}

fn put(data: &mut [u8], offset: usize, bytes: &[u8]) {
    data[offset..offset + bytes.len()].copy_from_slice(bytes);
}

fn put_u16(buf: &mut [u8], offset: usize, value: u16) {
    put(buf, offset, &value.to_le_bytes());
}

fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
    put(buf, offset, &value.to_le_bytes());
}

fn put_u64(buf: &mut [u8], offset: usize, value: u64) {
    put(buf, offset, &value.to_le_bytes());
}

fn node_with_next(node_type: u32, records: &[Vec<u8>], next_node: u32) -> Vec<u8> {
    let mut node = vec![0u8; NODE];
    put_u16(&mut node, 14, records.len() as u16);
    put_u32(&mut node, 24, next_node);
    put_u32(&mut node, 28, node_type);
    let mut position = 32;
    let mut offsets = Vec::new();
    for record in records {
        put(&mut node, position, record);
        offsets.push(position);
        position += record.len();
    }
    offsets.push(position);
    for (index, offset) in offsets.iter().enumerate() {
        put_u32(&mut node, NODE - 4 * (index + 2), *offset as u32);
    }
    node
}

fn head_node(root: u32, height: u16) -> Vec<u8> {
    let mut head = vec![0u8; NODE];
    put(&mut head, 0, b"eHND");
    put_u32(&mut head, 12, root);
    put_u16(&mut head, 16, height);
    head
}

fn leaf_record(
    parent: u64,
    object: u64,
    record_type: u8,
    name: &[u8],
    fork_bytes: &[u8],
) -> Vec<u8> {
    let key_length = KEY_FIXED_SIZE + name.len();
    let mut record = vec![0u8; key_length + fork_bytes.len()];
    let record_length = record.len();
    put_u16(&mut record, 4, key_length as u16);
    put_u16(&mut record, 6, record_length as u16);
    put_u64(&mut record, 8, parent);
    put_u64(&mut record, 16, object);
    record[24] = record_type;
    record[25] = name.len() as u8;
    put(&mut record, KEY_FIXED_SIZE, name);
    put(&mut record, key_length, fork_bytes);
    record
}

fn fork(size: u64, blocks: u64, extents: &[(u64, u64)]) -> Vec<u8> {
    let mut fork_bytes = vec![0u8; FORK_SIZE];
    put_u64(&mut fork_bytes, 0, size);
    put_u64(&mut fork_bytes, 8, blocks);
    for (position, (begin, length)) in extents.iter().enumerate() {
        put_u64(&mut fork_bytes, 16 + 16 * position, *begin);
        put_u64(&mut fork_bytes, 24 + 16 * position, *length);
    }
    fork_bytes
}

fn index_record(child: u32) -> Vec<u8> {
    let mut record = vec![0u8; 30];
    put_u16(&mut record, 4, 26);
    put_u16(&mut record, 6, 30);
    put_u32(&mut record, 26, child);
    record
}

fn overflow_record(object: u64, iblock: u64, begin: u64, length: u64) -> Vec<u8> {
    let mut record = vec![0u8; EXTENT_RECORD_SIZE];
    put_u16(&mut record, 4, EXTENT_KEY_SIZE as u16);
    put_u16(&mut record, 6, EXTENT_RECORD_SIZE as u16);
    put_u64(&mut record, 8, object);
    put_u64(&mut record, 16, iblock);
    put_u64(&mut record, 24, begin);
    put_u64(&mut record, 32, length);
    record
}

fn gzip(payload: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload).unwrap();
    encoder.finish().unwrap()
}

fn zlib(payload: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload).unwrap();
    encoder.finish().unwrap()
}

/// Appends a chunk table and a 32-byte descriptor, as in the Python test builder.
fn with_descriptor(chunk: &[u8], payload_len: usize, codec: &[u8; 3], layout: u16) -> Vec<u8> {
    let mut table = Vec::new();
    table.extend_from_slice(b"XT");
    table.extend_from_slice(&0u16.to_le_bytes());
    table.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
    table.extend_from_slice(&0u64.to_le_bytes());
    let mut descriptor = vec![0u8; COMPRESSED_DESCRIPTOR_SIZE];
    descriptor[0] = b'C';
    descriptor[1..4].copy_from_slice(codec);
    put_u16(&mut descriptor, 4, 1);
    put_u16(&mut descriptor, 6, layout);
    put_u64(&mut descriptor, 8, payload_len as u64);
    put_u32(&mut descriptor, 20, 17);
    let crc = kernel_crc32(&[&table[..], &descriptor[..]]);
    put_u32(&mut descriptor, COMPRESSED_CRC_OFFSET, crc);
    [chunk, &table[..], &descriptor[..]].concat()
}

/// Like `with_descriptor`, but uses the `I` marker and leaves `gap` padding bytes between the table
/// and the descriptor, as seen in the Samsung mkfs.vdfs blob.
fn with_padded_descriptor(
    chunk: &[u8],
    payload_len: usize,
    codec: &[u8; 3],
    gap: usize,
) -> Vec<u8> {
    let mut table = Vec::new();
    table.extend_from_slice(b"XT");
    table.extend_from_slice(&0u16.to_le_bytes());
    table.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
    table.extend_from_slice(&0u64.to_le_bytes());
    let padding = vec![0xA5u8; gap];
    let mut descriptor = vec![0u8; COMPRESSED_DESCRIPTOR_SIZE];
    descriptor[0] = b'I';
    descriptor[1..4].copy_from_slice(codec);
    put_u16(&mut descriptor, 4, 1);
    put_u16(&mut descriptor, 6, COMPRESSED_LAYOUT);
    put_u64(&mut descriptor, 8, payload_len as u64);
    let crc = kernel_crc32(&[&table[..], &padding[..], &descriptor[..]]);
    put_u32(&mut descriptor, COMPRESSED_CRC_OFFSET, crc);
    [chunk, &table[..], &padding[..], &descriptor[..]].concat()
}

fn build_image(catalog: &[Vec<u8>], overflow: &[Vec<u8>]) -> Vec<u8> {
    let mut data = vec![0u8; IMAGE_BLOCKS * BLOCK];

    let mut superblock = vec![0u8; 512];
    put(&mut superblock, 0, b"VDFS");
    put(&mut superblock, 4, b"1002");
    put_u64(&mut superblock, 8, IMAGE_BLOCKS as u64);
    superblock[164] = 12;
    superblock[165] = 14;
    let crc = kcrc(&superblock[..508]);
    put_u32(&mut superblock, 508, crc);
    put(&mut data, SUPERBLOCK, &superblock);

    let mut extended = vec![0u8; 2560];
    put_u64(&mut extended, 88, BASE_BLOCK as u64);
    put_u64(&mut extended, 96, 1);
    put_u64(&mut extended, 104, NODE_EXTENT_BLOCK as u64);
    put_u64(&mut extended, 112, 64);
    let crc = kcrc(&extended[..2556]);
    put_u32(&mut extended, 2556, crc);
    put(&mut data, EXTENDED, &extended);

    let mut base = vec![0u8; 1028];
    put(&mut base, 0, b"CoWB");
    put_u64(&mut base, 16, 1024);
    put_u64(&mut base, 24, 2);
    put_u64(&mut base, 64, BASE_RECORDS as u64);
    for index in 1..5 {
        put_u64(&mut base, 64 + 8 * index, EXTENT_TABLE as u64);
    }
    put_u64(&mut base, 40, 1);
    for (node_id, iblock) in [(0usize, 0u64), (1, 4), (2, 8)] {
        put_u64(&mut base, BASE_RECORDS + 16 * node_id, iblock);
    }
    for (node_id, iblock) in [(0usize, 12u64), (1, 16)] {
        put_u64(&mut base, EXTENT_TABLE + 16 * node_id, iblock);
    }
    let crc = kcrc(&base[..1024]);
    put_u32(&mut base, 1024, crc);
    put(&mut data, BASE_BLOCK * BLOCK, &base);

    put(&mut data, HEAD_NODE_OFFSET, &head_node(1, 2));
    put(
        &mut data,
        ROOT_NODE_OFFSET,
        &node_with_next(NODE_INDEX, &[index_record(2)], 0),
    );
    put(
        &mut data,
        LEAF_NODE_OFFSET,
        &node_with_next(NODE_LEAF, catalog, 0),
    );
    put(&mut data, EXTENT_HEAD_OFFSET, &head_node(1, 1));
    put(
        &mut data,
        EXTENT_LEAF_OFFSET,
        &node_with_next(NODE_LEAF, overflow, 0),
    );
    data
}

fn root_record() -> Vec<u8> {
    leaf_record(0, 1, RECORD_FOLDER, b"root", &[])
}

/// Write a POSIX mode at the record's mode offset, as the on-disk format stores it.
fn with_mode(mut record: Vec<u8>, mode: u16) -> Vec<u8> {
    let key_length = u16::from_le_bytes([record[4], record[5]]) as usize;
    put_u16(&mut record, key_length + RECORD_MODE_OFFSET, mode);
    record
}

fn readme_catalog(fork_bytes: &[u8]) -> Vec<Vec<u8>> {
    vec![
        root_record(),
        leaf_record(1, 9, RECORD_FILE, b"README.md", fork_bytes),
    ]
}

fn default_image() -> Vec<u8> {
    build_image(&readme_catalog(&fork(0, 0, &[])), &[])
}

fn readme_image(fork_bytes: &[u8], writes: Vec<(usize, Vec<u8>)>) -> Vec<u8> {
    let mut data = build_image(&readme_catalog(fork_bytes), &[]);
    for (block, bytes) in writes {
        put(&mut data, block * BLOCK, &bytes);
    }
    data
}

fn readme(data: &[u8]) -> CatalogRecord {
    Image::open(data)
        .unwrap()
        .catalog_records()
        .unwrap()
        .remove(1)
}

fn no_overflow() -> Overflow {
    HashMap::new()
}

fn error_text<T>(result: Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => format!("{error:#}"),
    }
}

fn rechecksum_base(data: &mut [u8]) {
    let start = BASE_BLOCK * BLOCK;
    let crc = kcrc(&data[start..start + 1024]);
    put_u32(data, start + 1024, crc);
}

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("vdfs1002-test-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    path
}

#[test]
fn kernel_crc_matches_python_vectors() {
    assert_eq!(kcrc(b"123456789"), 0x2dfd_2d88);
    assert_eq!(kcrc(b""), 0);
    assert_eq!(kcrc(b"hello world"), 0x66cd_a069);
}

#[test]
fn detects_image_by_magic() {
    assert!(is_vdfs1002(&default_image()));
    assert!(!is_vdfs1002(&[0u8; 2048]));
}

#[test]
fn lists_catalog_records() {
    let data = default_image();
    let image = Image::open(&data).unwrap();
    assert!(image.extended_checksum_valid);
    let records = image.catalog_records().unwrap();
    assert_eq!(
        records,
        vec![
            CatalogRecord {
                leaf_node: 2,
                parent_id: 0,
                object_id: 1,
                record_type: RECORD_FOLDER,
                name: b"root".to_vec(),
                mode: None,
                size: 0,
                blocks: 0,
                extents: vec![],
            },
            CatalogRecord {
                leaf_node: 2,
                parent_id: 1,
                object_id: 9,
                record_type: RECORD_FILE,
                name: b"README.md".to_vec(),
                mode: None,
                size: 0,
                blocks: 0,
                extents: vec![],
            },
        ]
    );
}

#[test]
fn audit_reports_reach_and_fork_consistency() {
    let data = readme_image(&fork(8000, 2, &[(20, 2)]), vec![]);
    let image = Image::open(&data).unwrap();
    let records = image.catalog_records().unwrap();
    let audit = image.audit(&records).unwrap();
    assert_eq!(audit.image_blocks, audit.declared_blocks);
    assert_eq!(audit.file_records, 1);
    assert_eq!(audit.fork_consistent, 1);
    assert_eq!(audit.max_inline_block_end, 22);
    assert_eq!(audit.files_resolved, 1);
    assert_eq!(audit.overflow_objects, 0);
    assert_eq!(audit.overflow_extents, 0);
    assert!(audit.max_node_block_end > 0);
    assert!(audit.meta_extent_end > 0);
}

#[test]
fn audit_flags_fork_size_mismatch() {
    let data = readme_image(&fork(100000, 2, &[(20, 2)]), vec![]);
    let image = Image::open(&data).unwrap();
    let records = image.catalog_records().unwrap();
    assert_eq!(image.audit(&records).unwrap().fork_consistent, 0);
}

#[test]
fn audit_counts_overflow_extents() {
    let data = build_image(
        &[
            root_record(),
            leaf_record(1, 9, RECORD_FILE, b"README.md", &fork(10000, 3, &[(3, 2)])),
        ],
        &[overflow_record(9, 2, 5, 1)],
    );
    let image = Image::open(&data).unwrap();
    let records = image.catalog_records().unwrap();
    let audit = image.audit(&records).unwrap();
    assert_eq!(audit.overflow_objects, 1);
    assert_eq!(audit.overflow_extents, 1);
    assert_eq!(audit.files_resolved, 1);
}

#[test]
fn selects_one_file_by_name_and_parent() {
    let data = default_image();
    let image = Image::open(&data).unwrap();
    let records = image.catalog_records().unwrap();
    assert_eq!(
        select_file(&records, "README.md", None).unwrap().object_id,
        9
    );
    assert_eq!(
        select_file(&records, "README.md", Some(1))
            .unwrap()
            .object_id,
        9
    );
    assert!(select_file(&records, "README.md", Some(7)).is_err());
    assert!(select_file(&records, "missing", None).is_err());
}

#[test]
fn record_json_uses_type_names() {
    let data = default_image();
    let image = Image::open(&data).unwrap();
    let records = image.catalog_records().unwrap();
    let json = records[1].to_json();
    assert_eq!(json["type"], "file");
    assert_eq!(json["name"], "README.md");
    assert_eq!(json["parent_id"], 1);
}

#[test]
fn bad_extended_checksum_is_informational() {
    let mut data = default_image();
    data[EXTENDED + 10] ^= 0xFF;
    assert!(!Image::open(&data).unwrap().extended_checksum_valid);
}

#[test]
fn rejects_bad_superblock_magic() {
    let mut data = default_image();
    put(&mut data, SUPERBLOCK, b"XXXX");
    assert!(error_text(Image::open(&data)).contains("not a VDFS1002 superblock"));
}

#[test]
fn rejects_superblock_checksum_mismatch() {
    let mut data = default_image();
    data[SUPERBLOCK + 10] ^= 0xFF;
    assert!(error_text(Image::open(&data)).contains("superblock checksum mismatch"));
}

#[test]
fn rejects_bad_base_table_magic() {
    let mut data = default_image();
    put(&mut data, BASE_BLOCK * BLOCK, b"XXXX");
    assert!(error_text(Image::open(&data)).contains("base table magic"));
}

#[test]
fn rejects_base_table_checksum_mismatch() {
    let mut data = default_image();
    data[BASE_BLOCK * BLOCK + 30] ^= 0xFF;
    assert!(error_text(Image::open(&data)).contains("base table checksum mismatch"));
}

#[test]
fn rejects_bad_head_magic() {
    let mut data = default_image();
    put(&mut data, HEAD_NODE_OFFSET, b"XXXX");
    let image = Image::open(&data).unwrap();
    assert!(error_text(image.catalog_records()).contains("head magic not found for inode 3"));
}

#[test]
fn rejects_truncated_image() {
    let data = default_image();
    let image = Image::open(&data[..60000]).unwrap();
    assert!(image.catalog_records().is_err());
}

#[test]
fn rejects_node_block_outside_node_extent() {
    let mut data = default_image();
    put_u64(&mut data, BASE_BLOCK * BLOCK + BASE_RECORDS + 16 * 2, 100);
    rechecksum_base(&mut data);
    let image = Image::open(&data).unwrap();
    assert!(error_text(image.catalog_records()).contains("outside the node extent"));
}

#[test]
fn rejects_leaf_chain_loop() {
    let mut data = default_image();
    put(
        &mut data,
        LEAF_NODE_OFFSET,
        &node_with_next(NODE_LEAF, &readme_catalog(&fork(0, 0, &[])), 2),
    );
    let image = Image::open(&data).unwrap();
    assert!(error_text(image.catalog_records()).contains("loops"));
}

#[test]
fn decodes_file_fork_extents() {
    let data = readme_image(&fork(8000, 2, &[(20, 2)]), vec![]);
    let record = readme(&data);
    assert_eq!(record.size, 8000);
    assert_eq!(record.blocks, 2);
    assert_eq!(
        record.extents,
        vec![Extent {
            begin: 20,
            length: 2
        }]
    );
}

#[test]
fn rejects_file_record_too_short_for_fork() {
    let catalog = vec![
        root_record(),
        leaf_record(1, 9, RECORD_FILE, b"README.md", &[]),
    ];
    let data = build_image(&catalog, &[]);
    let image = Image::open(&data).unwrap();
    assert!(error_text(image.catalog_records()).contains("too short for a file fork"));
}

#[test]
fn decodes_overflow_extents_by_object_and_iblock() {
    let data = build_image(
        &readme_catalog(&fork(10000, 3, &[(3, 2)])),
        &[overflow_record(9, 2, 5, 1)],
    );
    let image = Image::open(&data).unwrap();
    let overflow = image.overflow_extents().unwrap();
    assert_eq!(
        overflow.get(&9),
        Some(&vec![(
            2,
            Extent {
                begin: 5,
                length: 1
            }
        )])
    );
    let record = readme(&data);
    assert_eq!(
        file_extents(&record, &overflow).unwrap(),
        vec![
            Extent {
                begin: 3,
                length: 2
            },
            Extent {
                begin: 5,
                length: 1
            }
        ]
    );
}

#[test]
fn skips_zero_length_overflow_records() {
    let data = build_image(
        &readme_catalog(&fork(0, 0, &[])),
        &[overflow_record(9, 0, 0, 0)],
    );
    assert!(
        Image::open(&data)
            .unwrap()
            .overflow_extents()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn rejects_overflow_gap() {
    let data = build_image(
        &readme_catalog(&fork(10000, 3, &[(3, 2)])),
        &[overflow_record(9, 3, 5, 1)],
    );
    let overflow = Image::open(&data).unwrap().overflow_extents().unwrap();
    let record = readme(&data);
    assert!(error_text(file_extents(&record, &overflow)).contains("does not follow block 2"));
}

#[test]
fn reads_declared_bytes_across_extents() {
    let data = readme_image(
        &fork(10000, 3, &[(3, 2), (5, 1)]),
        vec![(3, vec![b'A'; 2 * BLOCK]), (5, vec![b'B'; BLOCK])],
    );
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    let expected = [vec![b'A'; 2 * BLOCK], vec![b'B'; 10000 - 2 * BLOCK]].concat();
    assert_eq!(
        image
            .read_file(&record, &no_overflow(), MAX_READ_BYTES)
            .unwrap(),
        expected
    );
}

#[test]
fn rejects_extents_short_of_declared_size() {
    let data = readme_image(&fork(10000, 2, &[(3, 2)]), vec![]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert!(
        error_text(image.read_file(&record, &no_overflow(), MAX_READ_BYTES))
            .contains("bytes short of the declared size")
    );
}

#[test]
fn refuses_extents_in_metadata_area() {
    let data = readme_image(&fork(8000, 2, &[(15, 2)]), vec![]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert!(
        error_text(image.read_file(&record, &no_overflow(), MAX_READ_BYTES))
            .contains("reaches the metadata area")
    );
}

#[test]
fn inflates_gzip_file_with_padded_table_and_i_marker() {
    let payload = b"padded table ".repeat(80);
    let blob = with_padded_descriptor(&gzip(&payload), payload.len(), b"Gzp", 320);
    let data = readme_image(&fork(blob.len() as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert_eq!(
        image.file_contents(&record, &no_overflow()).unwrap(),
        payload
    );
}

#[test]
fn inflates_file_with_wide_unaligned_table_padding() {
    let payload = b"wide padding ".repeat(90);
    let blob = with_padded_descriptor(&gzip(&payload), payload.len(), b"Gzp", 1264 + 7);
    let data = readme_image(&fork(blob.len() as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert_eq!(
        image.file_contents(&record, &no_overflow()).unwrap(),
        payload
    );
}

#[test]
fn rejects_corrupted_table_behind_padding() {
    let payload = b"padded guard ".repeat(40);
    let mut blob = with_padded_descriptor(&gzip(&payload), payload.len(), b"Gzp", 320);
    let len = blob.len();
    blob[len - COMPRESSED_DESCRIPTOR_SIZE - 320 - COMPRESSED_TABLE_ENTRY + 4] ^= 0x01;
    let data = readme_image(&fork(len as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert!(
        error_text(image.file_contents(&record, &no_overflow()))
            .contains("descriptor CRC mismatch")
    );
}

#[test]
fn inflates_gzip_compressed_file() {
    let payload = b"hello world ".repeat(100);
    let blob = with_descriptor(&gzip(&payload), payload.len(), b"Gzp", COMPRESSED_LAYOUT);
    let data = readme_image(&fork(blob.len() as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert_eq!(
        image.file_contents(&record, &no_overflow()).unwrap(),
        payload
    );
}

#[test]
fn inflates_zlib_compressed_file() {
    let payload = b"zlib payload ".repeat(50);
    let blob = with_descriptor(&zlib(&payload), payload.len(), b"Zip", COMPRESSED_LAYOUT);
    let data = readme_image(&fork(blob.len() as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert_eq!(
        image.file_contents(&record, &no_overflow()).unwrap(),
        payload
    );
}

#[test]
fn inflates_lzo_compressed_file() {
    // liblzo2 output for b"VDFS block " * 50.
    let stream = hex("085644465320626c6f636b202000ec28000c6f636b205644465320626c6f636b20110000");
    let payload = b"VDFS block ".repeat(50);
    let blob = with_descriptor(&stream, payload.len(), b"Lzo", COMPRESSED_LAYOUT);
    let data = readme_image(&fork(blob.len() as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert_eq!(
        image.file_contents(&record, &no_overflow()).unwrap(),
        payload
    );
}

#[test]
fn rejects_lzo_chunk_longer_than_declared_size() {
    let stream = hex("085644465320626c6f636b202000ec28000c6f636b205644465320626c6f636b20110000");
    let blob = with_descriptor(&stream, 100, b"Lzo", COMPRESSED_LAYOUT);
    let data = readme_image(&fork(blob.len() as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert!(
        error_text(image.file_contents(&record, &no_overflow()))
            .contains("is truncated or corrupt")
    );
}

#[test]
fn rejects_zstd_compressed_file() {
    let blob = with_descriptor(b"\x28\xb5\x2f\xfd", 4, b"Zst", COMPRESSED_LAYOUT);
    let data = readme_image(&fork(blob.len() as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert!(
        error_text(image.file_contents(&record, &no_overflow())).contains("Zst is not supported")
    );
}

#[test]
fn returns_plain_file_unchanged() {
    let data = readme_image(&fork(10, 1, &[(3, 1)]), vec![(3, b"0123456789".to_vec())]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert_eq!(
        image.file_contents(&record, &no_overflow()).unwrap(),
        b"0123456789"
    );
}

#[test]
fn rejects_truncated_compressed_chunk() {
    let payload = b"x".repeat(400);
    let gz = gzip(&payload);
    let blob = with_descriptor(
        &gz[..gz.len() - 8],
        payload.len(),
        b"Gzp",
        COMPRESSED_LAYOUT,
    );
    let data = readme_image(&fork(blob.len() as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert!(
        error_text(image.file_contents(&record, &no_overflow()))
            .contains("is truncated or corrupt")
    );
}

#[test]
fn rejects_compressed_descriptor_crc_mismatch() {
    let payload = b"crc guarded ".repeat(40);
    let mut blob = with_descriptor(&gzip(&payload), payload.len(), b"Gzp", COMPRESSED_LAYOUT);
    let len = blob.len();
    blob[len - COMPRESSED_DESCRIPTOR_SIZE + COMPRESSED_CRC_OFFSET] ^= 0xFF;
    let data = readme_image(&fork(len as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert!(
        error_text(image.file_contents(&record, &no_overflow()))
            .contains("descriptor CRC mismatch")
    );
}

#[test]
fn rejects_corrupted_chunk_table_before_inflating() {
    let payload = b"table guarded ".repeat(40);
    let mut blob = with_descriptor(&gzip(&payload), payload.len(), b"Gzp", COMPRESSED_LAYOUT);
    let len = blob.len();
    blob[len - COMPRESSED_DESCRIPTOR_SIZE - COMPRESSED_TABLE_ENTRY + 4] ^= 0x01;
    let data = readme_image(&fork(len as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert!(
        error_text(image.file_contents(&record, &no_overflow()))
            .contains("descriptor CRC mismatch")
    );
}

#[test]
fn rejects_unsupported_descriptor_layout() {
    let blob = with_descriptor(&gzip(b"abc"), 3, b"Gzp", 5);
    let data = readme_image(&fork(blob.len() as u64, 1, &[(3, 1)]), vec![(3, blob)]);
    let image = Image::open(&data).unwrap();
    let record = readme(&data);
    assert!(
        error_text(image.file_contents(&record, &no_overflow()))
            .contains("unsupported compressed descriptor layout 5")
    );
}

#[test]
fn restore_writes_tree_and_refuses_existing_destination() {
    let data = default_image();
    let destination = scratch("restore");
    let summary = restore(&data, &destination).unwrap();
    assert_eq!(summary.directories, 1);
    assert_eq!(summary.files, 1);
    assert!(summary.failures.is_empty());
    assert!(destination.join("root").join("README.md").is_file());
    assert!(restore(&data, &destination).is_err());
    fs::remove_dir_all(&destination).unwrap();
}

#[test]
fn parses_mode_and_leaves_it_absent_when_record_is_too_short() {
    let catalog = vec![
        root_record(),
        with_mode(
            leaf_record(1, 9, RECORD_FILE, b"README.md", &fork(0, 0, &[])),
            0o100755,
        ),
    ];
    let data = build_image(&catalog, &[]);
    let records = Image::open(&data).unwrap().catalog_records().unwrap();
    assert_eq!(records[0].mode, None);
    assert_eq!(records[1].mode, Some(0o100755));
}

#[cfg(unix)]
#[test]
fn restore_applies_file_and_directory_modes() {
    let catalog = vec![
        with_mode(leaf_record(0, 1, RECORD_FOLDER, b"root", &[0; 40]), 0o40500),
        with_mode(
            leaf_record(1, 9, RECORD_FILE, b"README.md", &fork(0, 0, &[])),
            0o100640,
        ),
    ];
    let data = build_image(&catalog, &[]);
    let destination = scratch("restore-modes");
    let summary = restore(&data, &destination).unwrap();
    assert!(summary.failures.is_empty());
    let root = destination.join("root");
    let mode_of = |path: PathBuf| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode_of(root.join("README.md")), 0o640);
    assert_eq!(mode_of(root.clone()), 0o500);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_dir_all(&destination).unwrap();
}

#[cfg(unix)]
#[test]
fn restore_creates_symlinks_for_link_mode_bodies() {
    let link_fork = fork(7, 1, &[(3, 1)]);
    let catalog = vec![
        root_record(),
        with_mode(
            leaf_record(1, 9, RECORD_FILE, b"ld.so", &link_fork),
            0o120777,
        ),
    ];
    let mut data = build_image(&catalog, &[]);
    put(&mut data, 3 * BLOCK, b"libc.so");
    let destination = scratch("symlinks");
    let summary = restore(&data, &destination).unwrap();
    assert!(summary.failures.is_empty());
    let link = destination.join("root").join("ld.so");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("libc.so"));
    fs::remove_dir_all(&destination).unwrap();
}

#[test]
fn restore_creates_hardlink_names_from_nameless_bodies() {
    let body_fork = fork(4, 1, &[(3, 1)]);
    let catalog = vec![
        root_record(),
        leaf_record(1, 2, RECORD_FOLDER, b"usr", &[]),
        leaf_record(50, 50, RECORD_FILE, b"", &body_fork),
        leaf_record(2, 50, RECORD_HARDLINK, b"libfoo.so", &[]),
        leaf_record(2, 77, RECORD_HARDLINK, b"dangling", &[]),
        leaf_record(2, 60, RECORD_INDIRECT_LINK, b"ilink", &[]),
        leaf_record(51, 1, RECORD_FILE, b"", &body_fork),
    ];
    let mut data = build_image(&catalog, &[]);
    put(&mut data, 3 * BLOCK, b"body");
    let destination = scratch("hardlinks");
    let summary = restore(&data, &destination).unwrap();
    assert_eq!(summary.hardlinks, 1);
    assert_eq!(summary.files, 0);
    assert_eq!(summary.links_not_created, 2);
    assert_eq!(summary.directories, 2);
    assert_eq!(summary.nameless_bodies, 1);
    assert_eq!(summary.skipped, 1);
    assert_eq!(
        fs::read(destination.join("root/usr/libfoo.so")).unwrap(),
        b"body"
    );
    assert!(!destination.join("root/usr/dangling").exists());
    fs::remove_dir_all(&destination).unwrap();
}

#[test]
fn restore_treats_indirect_records_as_back_references() {
    let catalog = vec![
        root_record(),
        leaf_record(1, 2, RECORD_FOLDER, b"usr", &[]),
        leaf_record(2, 3, RECORD_FOLDER, b"lib", &[]),
        leaf_record(3, 2, RECORD_INDIRECT_LINK, b"lib", &[]),
        leaf_record(1, 3, RECORD_INDIRECT_LINK, b"lib-alias", &[]),
    ];
    let data = build_image(&catalog, &[]);
    let destination = scratch("backrefs");
    let summary = restore(&data, &destination).unwrap();
    assert_eq!(summary.back_references, 1);
    assert_eq!(summary.links_not_created, 1);
    assert!(destination.join("root/usr/lib").is_dir());
    assert!(!destination.join("root/lib-alias").exists());
    fs::remove_dir_all(&destination).unwrap();
}

#[test]
fn restore_counts_back_references_on_file_bodies() {
    let catalog = vec![
        root_record(),
        leaf_record(1, 2, RECORD_FOLDER, b"usr", &[]),
        leaf_record(2, 70, RECORD_FILE, b"a.so", &fork(4, 1, &[(3, 1)])),
        leaf_record(70, 2, RECORD_INDIRECT_LINK, b"a.so", &[]),
    ];
    let mut data = build_image(&catalog, &[]);
    put(&mut data, 3 * BLOCK, b"body");
    let destination = scratch("backref-body");
    let summary = restore(&data, &destination).unwrap();
    assert_eq!(summary.back_references, 1);
    assert_eq!(summary.links_not_created, 0);
    assert_eq!(
        fs::read(destination.join("root/usr/a.so")).unwrap(),
        b"body"
    );
    fs::remove_dir_all(&destination).unwrap();
}

#[test]
fn unreadable_body_is_recorded_without_aborting_restore() {
    let catalog = vec![
        root_record(),
        leaf_record(1, 5, RECORD_FILE, b"a", &fork(4, 1, &[(3, 1)])),
        leaf_record(1, 6, RECORD_FILE, b"b", &fork(10000, 2, &[(3, 2)])),
    ];
    let mut data = build_image(&catalog, &[]);
    put(&mut data, 3 * BLOCK, b"body");
    let destination = scratch("unreadable");
    let summary = restore(&data, &destination).unwrap();
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].0, 6);
    assert_eq!(fs::read(destination.join("root/a")).unwrap(), b"body");
    fs::remove_dir_all(&destination).unwrap();
}

/// Survey of every file body in a real image. Run with `VDFS1002_SURVEY_IMAGE=<image>` and
/// `cargo test --locked -- --ignored survey_compressed_descriptors --nocapture`.
#[test]
#[ignore = "needs VDFS1002_SURVEY_IMAGE pointing at a real image"]
fn survey_compressed_descriptors() {
    let path = std::env::var_os("VDFS1002_SURVEY_IMAGE")
        .expect("set VDFS1002_SURVEY_IMAGE to a VDFS1002 image");
    let data = fs::read(&path).unwrap();
    let image = Image::open(&data).unwrap();
    let records = image.catalog_records().unwrap();
    let overflow = image.overflow_extents().unwrap();
    let mut survey = Survey::default();
    for record in records.iter().filter(|r| r.record_type == RECORD_FILE) {
        let verdict = classify(&image, record, &overflow);
        survey.add(record.object_id, &record.name, verdict);
    }
    survey.print();
    assert_eq!(
        survey.hard_failures(),
        0,
        "bodies or descriptors failed; see the survey output"
    );
}

const HARD_CATEGORIES: [&str; 5] = [
    "read_error",
    "descriptor_error",
    "zst_unsupported",
    "crc_mismatch",
    "decode_error",
];

struct Verdict {
    category: &'static str,
    gap: Option<usize>,
    note: String,
}

impl Verdict {
    fn new(category: &'static str) -> Self {
        Self {
            category,
            gap: None,
            note: String::new(),
        }
    }

    fn with_note(category: &'static str, note: String) -> Self {
        Self {
            category,
            gap: None,
            note,
        }
    }
}

/// Mirrors `file_contents`: a body is decompressed only when `compressed_descriptor` accepts it.
/// Anything else is kept raw, so a marker that is not recognised is reported here.
fn classify(image: &Image<'_>, record: &CatalogRecord, overflow: &Overflow) -> Verdict {
    let blob = match image.read_file(record, overflow, MAX_READ_BYTES) {
        Ok(blob) => blob,
        Err(error) => return Verdict::with_note("read_error", format!("{error:#}")),
    };
    let Some(start) = blob.len().checked_sub(COMPRESSED_DESCRIPTOR_SIZE) else {
        return Verdict::new("raw");
    };
    let marker = blob[start];
    let codec = &blob[start + 1..start + 4];
    if !matches!(marker, b'C' | b'I') {
        if blob.starts_with(&[0x1f, 0x8b]) {
            return Verdict::with_note("raw_gzip_magic", "gzip magic, no descriptor".into());
        }
        let tail = &blob[blob.len().saturating_sub(64 << 10)..];
        if let Some(pos) = tail.windows(4).position(|w| {
            matches!(w[0], b'C' | b'I') && matches!(&w[1..4], b"Zip" | b"Gzp" | b"Lzo" | b"Zst")
        }) {
            return Verdict::with_note(
                "raw_descriptor_candidate",
                format!("codec pattern at {} bytes before end", tail.len() - pos),
            );
        }
        return Verdict::new("raw");
    }
    if !matches!(codec, b"Zip" | b"Gzp" | b"Lzo" | b"Zst") {
        return Verdict::with_note(
            "marker_unknown_codec",
            format!(
                "marker {:?} codec {:?} len {} head {:02x?}",
                marker as char,
                codec,
                blob.len(),
                &blob[..blob.len().min(4)]
            ),
        );
    }
    let descriptor = match compressed_descriptor(&blob) {
        Ok(Some(descriptor)) => descriptor,
        Ok(None) => return Verdict::new("raw"),
        Err(error) => return Verdict::with_note("descriptor_error", format!("{error:#}")),
    };
    if &descriptor.codec == b"Zst" {
        return Verdict::new("zst_unsupported");
    }
    let count = descriptor.count as usize;
    let table = match locate_chunk_table(&blob, descriptor.start, count) {
        Ok(table) => table,
        Err(error) => return Verdict::with_note("crc_mismatch", format!("{error:#}")),
    };
    let latest = descriptor
        .start
        .saturating_sub(COMPRESSED_TABLE_ENTRY * count);
    let gap = latest.saturating_sub(table);
    match decompress_blob(&blob, &descriptor) {
        Ok(out) => Verdict {
            category: if out.starts_with(&[0x1f, 0x8b]) {
                "decoded_gzip_payload"
            } else {
                "decoded"
            },
            gap: Some(gap),
            note: String::new(),
        },
        Err(error) => Verdict::with_note("decode_error", format!("{error:#}")),
    }
}

#[derive(Default)]
struct Survey {
    bodies: u64,
    counts: std::collections::BTreeMap<&'static str, u64>,
    gaps: std::collections::BTreeMap<usize, u64>,
    notes: Vec<String>,
}

impl Survey {
    fn add(&mut self, object_id: u64, name: &[u8], verdict: Verdict) {
        self.bodies += 1;
        *self.counts.entry(verdict.category).or_default() += 1;
        if let Some(gap) = verdict.gap {
            *self.gaps.entry(gap).or_default() += 1;
        }
        let flagged = HARD_CATEGORIES.contains(&verdict.category)
            || matches!(
                verdict.category,
                "raw_gzip_magic" | "marker_unknown_codec" | "raw_descriptor_candidate"
            );
        if flagged {
            self.notes.push(format!(
                "{}\tobject {object_id}\t{}\t{}",
                verdict.category,
                String::from_utf8_lossy(name),
                verdict.note
            ));
        }
    }

    fn hard_failures(&self) -> u64 {
        HARD_CATEGORIES
            .iter()
            .map(|category| self.counts.get(category).copied().unwrap_or(0))
            .sum()
    }

    fn print(&self) {
        println!("bodies: {}", self.bodies);
        for (category, count) in &self.counts {
            println!("{category}: {count}");
        }
        println!("table gap (bytes: decoded descriptors):");
        for (gap, count) in &self.gaps {
            println!("  {gap}: {count}");
        }
        for note in &self.notes {
            println!("{note}");
        }
    }
}
