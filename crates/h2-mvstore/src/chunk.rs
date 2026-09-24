use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use crate::page::Page;
use h2_types::{H2Error, H2Result};

/// チャンクメタデータ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMeta {
    pub id: u32,
    pub version: u64,
    pub page_count: u32,
    pub data_size: u64,
}

/// ディスク上に追記シリアライズされるチャンク単位のデータ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkPayload {
    pub meta: ChunkMeta,
    pub root_page: Page,
}

impl ChunkPayload {
    pub fn new(id: u32, version: u64, root: Page) -> Self {
        Self {
            meta: ChunkMeta {
                id,
                version,
                page_count: 1,
                data_size: 0,
            },
            root_page: root,
        }
    }

    /// バイト列へシリアライズ（末尾に4バイトのCRC32チェックサムを付与）
    pub fn serialize(&self) -> H2Result<Vec<u8>> {
        let serialized = serde_json::to_vec(self)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;

        let mut hasher = Hasher::new();
        hasher.update(&serialized);
        let checksum = hasher.finalize();

        let mut buffer = Vec::with_capacity(4 + serialized.len() + 4);
        buffer.extend_from_slice(&(serialized.len() as u32).to_le_bytes());
        buffer.extend_from_slice(&serialized);
        buffer.extend_from_slice(&checksum.to_le_bytes());

        Ok(buffer)
    }

    /// バイト列からデシリアライズ（CRC32チェックサムを検証）
    pub fn deserialize(data: &[u8]) -> H2Result<Self> {
        if data.len() < 8 {
            return Err(H2Error::Corrupted("Chunk data too short".to_string()));
        }

        let length = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < 4 + length + 4 {
            return Err(H2Error::Corrupted("Chunk buffer size mismatch".to_string()));
        }

        let payload_bytes = &data[4..4 + length];
        let checksum_bytes = &data[4 + length..4 + length + 4];
        let expected_checksum = u32::from_le_bytes(checksum_bytes.try_into().unwrap());

        let mut hasher = Hasher::new();
        hasher.update(payload_bytes);
        let actual_checksum = hasher.finalize();

        if expected_checksum != actual_checksum {
            return Err(H2Error::Corrupted(format!(
                "CRC32 mismatch: expected {}, got {}",
                expected_checksum, actual_checksum
            )));
        }

        let payload: ChunkPayload = serde_json::from_slice(payload_bytes)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;

        Ok(payload)
    }
}
