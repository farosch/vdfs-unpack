pub const SECTOR_SIZE: usize = 512;
pub const VDFS4_EXTENTS_COUNT_IN_FORK: usize = 9;
pub const VDFS4_INLINE_DATA_MAX_SIZE: usize = 216;
pub const VDFS4_META_BTREE_EXTENTS: usize = 96;
pub const VDFS4_SF_NR: usize = 5;
pub const VDFS4_BNODE_FIRST_OFFSET: usize = 4;

pub const VDFS4_SB_SIGNATURE: &[u8; 4] = b"VDFS";
pub const VDFS4_LAYOUT_VERSION_2007: &[u8; 4] = b"2007";
pub const VDFS4_LAYOUT_VERSION_2006: &[u8; 4] = b"2006";
pub const VDFS4_BTREE_HEAD_NODE_MAGIC: &[u8; 4] = b"eHND";

pub const VDFS4_NODE_LEAF: u32 = 2;

pub const VDFS4_CATALOG_FOLDER_RECORD: u8 = 0x01;
pub const VDFS4_CATALOG_FILE_RECORD: u8 = 0x02;
pub const VDFS4_CATALOG_HLINK_RECORD: u8 = 0x03;
pub const VDFS4_CATALOG_ILINK_RECORD: u8 = 0x05;

pub const VDFS4_ROOTDIR_OBJ_ID: u64 = 0;
pub const VDFS4_CAT_TREE_INO: u64 = 3;
pub const VDFS4_EXTENTS_TREE_INO: u64 = 5;
pub const VDFS4_XATTR_TREE_INO: u64 = 7;

pub const VDFS4_CHUNK_FLAG_UNCOMPR: u16 = 0x1;
pub const VDFS4_CHUNK_FLAG_ENCRYPTED: u16 = 0x2;
pub const VDFS4_AES_NONCE_SIZE: usize = 8;

pub const VDFS4_COMPRESSED_FILE: u32 = 1 << 13;

pub const VDFS4_COMPR_FILE_DESC_LEN_06: usize = 5 * 8; // 40 bytes (layout ver 06)
pub const VDFS4_COMPR_FILE_DESC_LEN_05: usize = 4 * 8; // 32 bytes (layout ver 05)
pub const VDFS4_COMPR_LAYOUT_VER_05: u16 = 0x0005;
pub const VDFS4_COMPR_LAYOUT_VER_06: u16 = 0x0006;

#[derive(Clone, Copy, Debug, Default)]
#[repr(C, packed)]
pub struct Vdfs4Timespec {
    pub seconds: u32,
    pub seconds_high: u32,
    pub nanoseconds: u32,
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C, packed)]
pub struct Vdfs4Extent {
    pub begin: u64,
    pub length: u64,
}

impl Vdfs4Extent {
    pub fn is_empty(self) -> bool {
        self.begin == 0 && self.length == 0
    }
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C, packed)]
pub struct Vdfs4IExtent {
    pub extent: Vdfs4Extent,
    pub iblock: u64,
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4Fork {
    pub size_in_bytes: u64,
    pub total_blocks_count: u64,
    pub data: [u8; VDFS4_INLINE_DATA_MAX_SIZE],
}

impl Default for Vdfs4Fork {
    fn default() -> Self {
        Self {
            size_in_bytes: 0,
            total_blocks_count: 0,
            data: [0; VDFS4_INLINE_DATA_MAX_SIZE],
        }
    }
}

impl Vdfs4Fork {
    pub fn extents(&self) -> [Vdfs4IExtent; VDFS4_EXTENTS_COUNT_IN_FORK] {
        let mut result = [Vdfs4IExtent {
            extent: Vdfs4Extent {
                begin: 0,
                length: 0,
            },
            iblock: 0,
        }; VDFS4_EXTENTS_COUNT_IN_FORK];
        for (i, iext) in result.iter_mut().enumerate() {
            let off = i * std::mem::size_of::<Vdfs4IExtent>();
            iext.extent.begin = read_u64_le(&self.data, off);
            iext.extent.length = read_u64_le(&self.data, off + 8);
            iext.iblock = read_u64_le(&self.data, off + 16);
        }
        result
    }

    pub fn inline_data(&self) -> Vec<u8> {
        self.data.to_vec()
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4SuperBlock {
    pub signature: [u8; 4],
    pub layout_version: [u8; 4],
    pub maximum_blocks_count: u64,
    pub creation_timestamp: Vdfs4Timespec,
    pub volume_uuid: [u8; 16],
    pub volume_name: [u8; 16],
    pub mkfs_version: [u8; 64],
    pub unused: [u8; 40],
    pub log_block_size: u8,
    pub log_super_page_size: u8,
    pub log_erase_block_size: u8,
    pub case_insensitive: u8,
    pub read_only: u8,
    pub image_crc32_present: u8,
    pub force_full_decomp_decrypt: u8,
    pub hash_type: u8,
    pub encryption_flags: u8,
    pub sign_type: u8,
    pub reserved: [u8; 54],
    pub exsb_checksum: u32,
    pub basetable_checksum: u32,
    pub meta_hashtable_checksum: u32,
    pub image_inode_count: u64,
    pub pad: u32,
    pub sb_hash: [u8; 256],
    pub checksum: u32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4ExtendedSuperBlock {
    pub files_count: u64,
    pub folders_count: u64,
    pub volume_body: Vdfs4Extent,
    pub mount_counter: u32,
    pub sync_counter: u32,
    pub umount_counter: u32,
    pub generation: u32,
    pub debug_area: Vdfs4Extent,
    pub meta_tbc: u32,
    pub pad: u32,
    pub tables: Vdfs4Extent,
    pub meta: [Vdfs4Extent; VDFS4_META_BTREE_EXTENTS],
    pub extension: Vdfs4Extent,
    pub volume_blocks_count: u64,
    pub crc: u8,
    pub volume_uuid: [u8; 16],
    pub _reserved: [u8; 7],
    pub kbytes_written: u64,
    pub meta_hashtable_area: Vdfs4Extent,
    pub reserved: [u8; 860],
    pub checksum: u32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4LayoutSb {
    pub sb1: Vdfs4SuperBlock,
    pub sb2: Vdfs4SuperBlock,
    pub sb: Vdfs4SuperBlock,
    pub exsb: Vdfs4ExtendedSuperBlock,
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4CatalogFolderRecord {
    pub flags: u32,
    pub generation: u32,
    pub total_items_count: u64,
    pub links_count: u64,
    pub next_orphan_id: u64,
    pub file_mode: u16,
    pub pad: u16,
    pub uid: u32,
    pub gid: u32,
    pub creation_time: Vdfs4Timespec,
    pub modification_time: Vdfs4Timespec,
    pub access_time: Vdfs4Timespec,
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4CatalogFileRecord {
    pub common: Vdfs4CatalogFolderRecord,
    pub fork: Vdfs4Fork,
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4RawBtreeHead {
    pub magic: [u8; 4],
    pub version: [u32; 2],
    pub root_bnode_id: u32,
    pub btree_height: u16,
    pub padding: [u8; 2],
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4SnapshotDescriptor {
    pub signature: [u8; 4],
    pub sync_count: u32,
    pub mount_count: u64,
    pub checksum_offset: u64,
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4BaseTable {
    pub descriptor: Vdfs4SnapshotDescriptor,
    pub last_page_index: [u64; VDFS4_SF_NR],
    pub translation_table_offsets: [u64; VDFS4_SF_NR],
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4BaseTableRecord {
    pub meta_iblock: u64,
    pub sync_count: u32,
    pub mount_count: u32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4CompFileDescr {
    pub reserved: [u8; 7],
    pub sign_type: u8,
    pub magic: [u8; 4],
    pub extents_num: u16,
    pub layout_version: u16,
    pub unpacked_size: u64,
    pub crc: u32,
    pub log_chunk_size: u32,
    pub aes_nonce: [u8; VDFS4_AES_NONCE_SIZE],
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Vdfs4CompExtent {
    pub magic_or_index: u16,
    pub flags: u16,
    pub len_bytes: u32,
    pub start: u64,
}

pub fn sf_index(ino: u64) -> usize {
    (ino - VDFS4_CAT_TREE_INO) as usize
}

pub unsafe fn read_packed<T: Copy>(data: &[u8], offset: usize) -> Option<T> {
    let size = std::mem::size_of::<T>();
    if offset + size > data.len() {
        return None;
    }
    Some(unsafe { std::ptr::read_unaligned(data[offset..].as_ptr() as *const T) })
}

pub fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

pub fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

pub fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

pub const CATTREE_KEY_PARENT_ID_OFF: usize = 8;
pub const CATTREE_KEY_OBJECT_ID_OFF: usize = 16;
pub const CATTREE_KEY_RECORD_TYPE_OFF: usize = 24;
pub const CATTREE_KEY_NAME_LEN_OFF: usize = 25;
pub const CATTREE_KEY_NAME_OFF: usize = 26;

pub fn cattree_key_parent_id(data: &[u8]) -> u64 {
    read_u64_le(data, CATTREE_KEY_PARENT_ID_OFF)
}

pub fn cattree_key_object_id(data: &[u8]) -> u64 {
    read_u64_le(data, CATTREE_KEY_OBJECT_ID_OFF)
}

pub fn cattree_key_record_type(data: &[u8]) -> u8 {
    data[CATTREE_KEY_RECORD_TYPE_OFF]
}

pub fn cattree_key_name_len(data: &[u8]) -> u8 {
    data[CATTREE_KEY_NAME_LEN_OFF]
}

pub fn cattree_key_name(data: &[u8]) -> &[u8] {
    let len = cattree_key_name_len(data) as usize;
    if CATTREE_KEY_NAME_OFF + len <= data.len() {
        &data[CATTREE_KEY_NAME_OFF..CATTREE_KEY_NAME_OFF + len]
    } else {
        &[]
    }
}

pub fn cattree_key_key_len(data: &[u8]) -> usize {
    read_u16_le(data, 4) as usize
}

pub fn cattree_key_record_len(data: &[u8]) -> usize {
    read_u16_le(data, 6) as usize
}
