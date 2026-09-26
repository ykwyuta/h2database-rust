use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use crc32fast::Hasher;
use h2_types::{H2Error, H2Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const BACKUP_MAGIC: &[u8; 4] = b"H2BK";
pub const BACKUP_TRAILER_MAGIC: &[u8; 4] = b"H2EF";
pub const CURRENT_BACKUP_VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 64;
pub const TRAILER_SIZE: usize = 24;

/// バックアップファイルのメタデータ・マニフェスト情報
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupMetadata {
    pub format_version: u32,
    pub backup_id: String,
    pub snapshot_version: u64,
    pub timestamp_nanos: i64,
    pub is_replica: bool,
    pub total_maps: usize,
    pub total_records: u64,
    pub total_bytes: u64,
    pub checksum: u32,
}

impl BackupMetadata {
    pub fn timestamp_datetime(&self) -> String {
        let secs = (self.timestamp_nanos / 1_000_000_000) as u64;
        let d = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        format!("{:?}", d)
    }
}

/// 高速バイナリ形式でバックアップを出力
pub fn write_binary_backup<W: Write>(
    writer: &mut W,
    snapshot_version: u64,
    is_replica: bool,
    maps: &HashMap<String, Vec<(Vec<u8>, Vec<u8>)>>,
) -> H2Result<BackupMetadata> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let timestamp_nanos = now.as_nanos() as i64;

    let backup_id_bytes: [u8; 16] = {
        let mut id = [0u8; 16];
        let t_bytes = (timestamp_nanos as u64).to_le_bytes();
        id[..8].copy_from_slice(&t_bytes);
        let v_bytes = snapshot_version.to_le_bytes();
        id[8..].copy_from_slice(&v_bytes);
        id
    };
    let backup_id = hex::encode(backup_id_bytes);

    let total_maps = maps.len() as u32;

    // 1. ヘッダの生成 (64 bytes)
    // 0..4: Magic (4)
    // 4..8: Version (4)
    // 8..24: Backup ID (16)
    // 24..32: Snapshot Version (8)
    // 32..40: Timestamp Nanos (8)
    // 40: Source Role (1: 0=Primary, 1=Replica)
    // 41: Checksum Type (1: 1=CRC32)
    // 42..46: Total Maps (4)
    // 46..60: Reserved (14)
    // 60..64: Header Checksum (4)
    let mut header = [0u8; HEADER_SIZE];
    header[0..4].copy_from_slice(BACKUP_MAGIC);
    header[4..8].copy_from_slice(&CURRENT_BACKUP_VERSION.to_le_bytes());
    header[8..24].copy_from_slice(&backup_id_bytes);
    header[24..32].copy_from_slice(&snapshot_version.to_le_bytes());
    header[32..40].copy_from_slice(&timestamp_nanos.to_le_bytes());
    header[40] = if is_replica { 1 } else { 0 };
    header[41] = 1; // CRC32
    header[42..46].copy_from_slice(&total_maps.to_le_bytes());

    let header_checksum = {
        let mut h = Hasher::new();
        h.update(&header[0..60]);
        h.finalize()
    };
    header[60..64].copy_from_slice(&header_checksum.to_le_bytes());

    writer.write_all(&header)?;

    // 2. マップセクションの直列化
    let mut payload_hasher = Hasher::new();
    let mut total_records: u64 = 0;
    let mut payload_bytes: u64 = 0;

    // ソートして出力順序を決定的に保つ
    let mut sorted_map_names: Vec<&String> = maps.keys().collect();
    sorted_map_names.sort();

    for name in sorted_map_names {
        let entries = &maps[name];
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len() as u32;
        let entry_count = entries.len() as u64;

        let mut map_buf = Vec::new();
        map_buf.write_u32::<LittleEndian>(name_len)?;
        map_buf.write_all(name_bytes)?;
        map_buf.write_u64::<LittleEndian>(entry_count)?;

        let mut map_hasher = Hasher::new();
        map_hasher.update(&name_len.to_le_bytes());
        map_hasher.update(name_bytes);
        map_hasher.update(&entry_count.to_le_bytes());

        for (k, v) in entries {
            let k_len = k.len() as u32;
            let v_len = v.len() as u32;
            map_buf.write_u32::<LittleEndian>(k_len)?;
            map_buf.write_all(k)?;
            map_buf.write_u32::<LittleEndian>(v_len)?;
            map_buf.write_all(v)?;

            map_hasher.update(&k_len.to_le_bytes());
            map_hasher.update(k);
            map_hasher.update(&v_len.to_le_bytes());
            map_hasher.update(v);
            total_records += 1;
        }

        let map_crc = map_hasher.finalize();
        map_buf.write_u32::<LittleEndian>(map_crc)?;

        payload_hasher.update(&map_buf);
        payload_bytes += map_buf.len() as u64;
        writer.write_all(&map_buf)?;
    }

    let overall_checksum = payload_hasher.finalize();

    // 3. トレイラーの出力 (24 bytes)
    // 0..8: Total Data Bytes (8)
    // 8..12: Overall Checksum (4)
    // 12..20: Total Records (8)
    // 20..24: Trailer Magic (4)
    let mut trailer = [0u8; TRAILER_SIZE];
    trailer[0..8].copy_from_slice(&payload_bytes.to_le_bytes());
    trailer[8..12].copy_from_slice(&overall_checksum.to_le_bytes());
    trailer[12..20].copy_from_slice(&total_records.to_le_bytes());
    trailer[20..24].copy_from_slice(BACKUP_TRAILER_MAGIC);

    writer.write_all(&trailer)?;
    writer.flush()?;

    let total_bytes = HEADER_SIZE as u64 + payload_bytes + TRAILER_SIZE as u64;

    Ok(BackupMetadata {
        format_version: CURRENT_BACKUP_VERSION,
        backup_id,
        snapshot_version,
        timestamp_nanos,
        is_replica,
        total_maps: maps.len(),
        total_records,
        total_bytes,
        checksum: overall_checksum,
    })
}

/// バックアップファイルの検証（データ抽出なし）
pub fn verify_binary_backup<R: Read + Seek>(reader: &mut R) -> H2Result<BackupMetadata> {
    let (meta, _) = read_binary_backup_internal(reader, false)?;
    Ok(meta)
}

/// バックアップファイルの検証および全マップデータの抽出
pub fn read_and_verify_binary_backup<R: Read + Seek>(
    reader: &mut R,
) -> H2Result<(BackupMetadata, HashMap<String, Vec<(Vec<u8>, Vec<u8>)>>)> {
    let (meta, maps) = read_binary_backup_internal(reader, true)?;
    Ok((meta, maps.unwrap_or_default()))
}

fn read_binary_backup_internal<R: Read + Seek>(
    reader: &mut R,
    load_entries: bool,
) -> H2Result<(BackupMetadata, Option<HashMap<String, Vec<(Vec<u8>, Vec<u8>)>>>)> {
    reader.seek(SeekFrom::Start(0))?;

    // 1. ヘッダ読み込み & 検証
    let mut magic = [0u8; 4];
    match reader.read_exact(&mut magic) {
        Ok(_) => {
            if &magic != BACKUP_MAGIC {
                return Err(H2Error::Storage(format!(
                    "Invalid backup magic: expected {:?}, found {:?}",
                    BACKUP_MAGIC, &magic
                )));
            }
        }
        Err(e) => {
            return Err(H2Error::Storage(format!(
                "Invalid backup file: file too short or unreadable: {}",
                e
            )));
        }
    }

    let mut header = [0u8; HEADER_SIZE];
    header[0..4].copy_from_slice(&magic);
    reader.read_exact(&mut header[4..])?;

    let format_version = u32::from_le_bytes(header[4..8].try_into().unwrap());
    if format_version != CURRENT_BACKUP_VERSION {
        return Err(H2Error::Storage(format!(
            "Unsupported backup format version: {}",
            format_version
        )));
    }

    let header_checksum = u32::from_le_bytes(header[60..64].try_into().unwrap());
    let mut h = Hasher::new();
    h.update(&header[0..60]);
    let calc_header_checksum = h.finalize();
    if header_checksum != calc_header_checksum {
        return Err(H2Error::Storage(format!(
            "Backup header corruption: expected checksum 0x{:08X}, calculated 0x{:08X}",
            header_checksum, calc_header_checksum
        )));
    }

    let backup_id_bytes: [u8; 16] = header[8..24].try_into().unwrap();
    let backup_id = hex::encode(backup_id_bytes);
    let snapshot_version = u64::from_le_bytes(header[24..32].try_into().unwrap());
    let timestamp_nanos = i64::from_le_bytes(header[32..40].try_into().unwrap());
    let is_replica = header[40] != 0;
    let total_maps = u32::from_le_bytes(header[42..46].try_into().unwrap()) as usize;

    // 2. マップセクションの読み込み
    let mut payload_hasher = Hasher::new();
    let mut maps_data = if load_entries {
        Some(HashMap::with_capacity(total_maps))
    } else {
        None
    };

    let mut records_counted: u64 = 0;
    let mut payload_bytes_read: u64 = 0;

    for _ in 0..total_maps {
        let name_len = reader.read_u32::<LittleEndian>()?;
        payload_hasher.update(&name_len.to_le_bytes());
        payload_bytes_read += 4;

        let mut name_bytes = vec![0u8; name_len as usize];
        reader.read_exact(&mut name_bytes)?;
        payload_hasher.update(&name_bytes);
        payload_bytes_read += name_len as u64;

        let map_name = String::from_utf8(name_bytes)
            .map_err(|e| H2Error::Storage(format!("Invalid map name in backup: {}", e)))?;

        let entry_count = reader.read_u64::<LittleEndian>()?;
        payload_hasher.update(&entry_count.to_le_bytes());
        payload_bytes_read += 8;

        let mut map_entries = if load_entries {
            Some(Vec::with_capacity(entry_count.min(100_000) as usize))
        } else {
            None
        };

        let mut map_hasher = Hasher::new();
        map_hasher.update(&name_len.to_le_bytes());
        map_hasher.update(map_name.as_bytes());
        map_hasher.update(&entry_count.to_le_bytes());

        for _ in 0..entry_count {
            let k_len = reader.read_u32::<LittleEndian>()?;
            payload_hasher.update(&k_len.to_le_bytes());
            map_hasher.update(&k_len.to_le_bytes());
            payload_bytes_read += 4;

            let mut key = vec![0u8; k_len as usize];
            reader.read_exact(&mut key)?;
            payload_hasher.update(&key);
            map_hasher.update(&key);
            payload_bytes_read += k_len as u64;

            let v_len = reader.read_u32::<LittleEndian>()?;
            payload_hasher.update(&v_len.to_le_bytes());
            map_hasher.update(&v_len.to_le_bytes());
            payload_bytes_read += 4;

            let mut val = vec![0u8; v_len as usize];
            reader.read_exact(&mut val)?;
            payload_hasher.update(&val);
            map_hasher.update(&val);
            payload_bytes_read += v_len as u64;

            if let Some(ref mut entries) = map_entries {
                entries.push((key, val));
            }
            records_counted += 1;
        }

        let stored_map_crc = reader.read_u32::<LittleEndian>()?;
        payload_hasher.update(&stored_map_crc.to_le_bytes());
        payload_bytes_read += 4;

        let calculated_map_crc = map_hasher.finalize();
        if stored_map_crc != calculated_map_crc {
            return Err(H2Error::Storage(format!(
                "Checksum mismatch in map '{}': stored 0x{:08X}, calculated 0x{:08X}",
                map_name, stored_map_crc, calculated_map_crc
            )));
        }

        if let Some(ref mut m) = maps_data {
            m.insert(map_name, map_entries.unwrap_or_default());
        }
    }

    let calculated_overall_checksum = payload_hasher.finalize();

    // 3. トレイラーの検証
    let mut trailer = [0u8; TRAILER_SIZE];
    reader.read_exact(&mut trailer)?;

    if &trailer[20..24] != BACKUP_TRAILER_MAGIC {
        return Err(H2Error::Storage("Missing or corrupt backup trailer magic".to_string()));
    }

    let total_data_bytes = u64::from_le_bytes(trailer[0..8].try_into().unwrap());
    let stored_overall_checksum = u32::from_le_bytes(trailer[8..12].try_into().unwrap());
    let total_records = u64::from_le_bytes(trailer[12..20].try_into().unwrap());

    if total_data_bytes != payload_bytes_read {
        return Err(H2Error::Storage(format!(
            "Payload length mismatch: expected {}, read {}",
            total_data_bytes, payload_bytes_read
        )));
    }

    if stored_overall_checksum != calculated_overall_checksum {
        return Err(H2Error::Storage(format!(
            "Overall payload checksum corruption: expected 0x{:08X}, calculated 0x{:08X}",
            stored_overall_checksum, calculated_overall_checksum
        )));
    }

    if total_records != records_counted {
        return Err(H2Error::Storage(format!(
            "Record count mismatch: expected {}, read {}",
            total_records, records_counted
        )));
    }

    let total_bytes = HEADER_SIZE as u64 + payload_bytes_read + TRAILER_SIZE as u64;

    let meta = BackupMetadata {
        format_version,
        backup_id,
        snapshot_version,
        timestamp_nanos,
        is_replica,
        total_maps,
        total_records,
        total_bytes,
        checksum: stored_overall_checksum,
    };

    Ok((meta, maps_data))
}

/// バックアップファイルがバイナリ形式 (`H2BK`) か判別
pub fn is_binary_backup_file<P: AsRef<Path>>(path: P) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    if f.read_exact(&mut magic).is_ok() {
        &magic == BACKUP_MAGIC
    } else {
        false
    }
}

pub fn is_binary_backup_file_stream<R: Read + Seek>(reader: &mut R) -> std::io::Result<bool> {
    let pos = reader.stream_position()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    let is_bin = match reader.read_exact(&mut magic) {
        Ok(_) => &magic == BACKUP_MAGIC,
        Err(_) => false,
    };
    reader.seek(SeekFrom::Start(pos))?;
    Ok(is_bin)
}

/// 16進数文字列へのエンコード補助
mod hex {
    pub fn encode(bytes: [u8; 16]) -> String {
        let mut s = String::with_capacity(32);
        for b in bytes {
            use std::fmt::Write;
            let _ = write!(&mut s, "{:02x}", b);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_binary_backup_roundtrip() {
        let mut maps = HashMap::new();
        maps.insert(
            "users".to_string(),
            vec![
                (b"key1".to_vec(), b"Alice".to_vec()),
                (b"key2".to_vec(), b"Bob".to_vec()),
            ],
        );
        maps.insert(
            "orders".to_string(),
            vec![(b"o100".to_vec(), b"OrderData".to_vec())],
        );

        let mut buffer = Cursor::new(Vec::new());
        let meta = write_binary_backup(&mut buffer, 42, false, &maps).unwrap();

        assert_eq!(meta.snapshot_version, 42);
        assert_eq!(meta.total_maps, 2);
        assert_eq!(meta.total_records, 3);
        assert!(!meta.is_replica);

        // 検証のみ
        let verified_meta = verify_binary_backup(&mut buffer).unwrap();
        assert_eq!(meta, verified_meta);

        // データ抽出
        let (read_meta, restored_maps) = read_and_verify_binary_backup(&mut buffer).unwrap();
        assert_eq!(read_meta, meta);
        assert_eq!(restored_maps.len(), 2);
        assert_eq!(restored_maps["users"].len(), 2);
        assert_eq!(restored_maps["orders"].len(), 1);
        assert_eq!(restored_maps["users"][0], (b"key1".to_vec(), b"Alice".to_vec()));
    }

    #[test]
    fn test_binary_backup_corruption_detection() {
        let mut maps = HashMap::new();
        maps.insert("table1".to_string(), vec![(b"k".to_vec(), b"v".to_vec())]);

        let mut buffer = Cursor::new(Vec::new());
        write_binary_backup(&mut buffer, 10, true, &maps).unwrap();

        // 1. エントリデータを故意に破損させる (バリューのバイトを反転)
        let mut corrupted = buffer.into_inner();
        let corrupt_idx = 91; // value 'v'
        corrupted[corrupt_idx] ^= 0xFF;

        let mut read_cur = Cursor::new(corrupted.clone());
        let err = verify_binary_backup(&mut read_cur).unwrap_err();
        assert!(err.to_string().contains("Checksum mismatch") || err.to_string().contains("corruption"));

        // 2. ヘッダを破損させる
        corrupted[10] ^= 0xFF;
        let mut read_cur2 = Cursor::new(corrupted);
        let err2 = verify_binary_backup(&mut read_cur2).unwrap_err();
        assert!(err2.to_string().contains("corruption") || err2.to_string().contains("Checksum mismatch"));
    }
}
