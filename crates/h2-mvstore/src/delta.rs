use crc32fast::Hasher;
use h2_types::{H2Error, H2Result};
use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 4] = b"H2D1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapDelta {
    pub name: String,
    pub replace_root: Option<Vec<u8>>,
    pub clear: bool,
    pub changes: Vec<(Vec<u8>, Option<Vec<u8>>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaPayload {
    pub version: u64,
    pub previous_offset: u64,
    pub previous_length: u32,
    pub removed_maps: Vec<String>,
    pub maps: Vec<MapDelta>,
}

impl DeltaPayload {
    pub fn is_delta(data: &[u8]) -> bool {
        data.starts_with(MAGIC)
    }

    pub fn serialize(&self) -> H2Result<Vec<u8>> {
        let payload =
            bincode::serialize(self).map_err(|e| H2Error::Serialization(e.to_string()))?;
        let mut checksum = Hasher::new();
        checksum.update(&payload);
        let mut bytes = Vec::with_capacity(12 + payload.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&checksum.finalize().to_le_bytes());
        Ok(bytes)
    }

    pub fn deserialize(data: &[u8]) -> H2Result<Self> {
        if data.len() < 12 || !Self::is_delta(data) {
            return Err(H2Error::Corrupted("Invalid delta checkpoint".to_string()));
        }
        let length = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        if data.len() != length + 12 {
            return Err(H2Error::Corrupted(
                "Delta checkpoint size mismatch".to_string(),
            ));
        }
        let payload = &data[8..8 + length];
        let expected = u32::from_le_bytes(data[8 + length..].try_into().unwrap());
        let mut checksum = Hasher::new();
        checksum.update(payload);
        if checksum.finalize() != expected {
            return Err(H2Error::Corrupted(
                "Delta checkpoint checksum mismatch".to_string(),
            ));
        }
        bincode::deserialize(payload).map_err(|e| H2Error::Serialization(e.to_string()))
    }
}
