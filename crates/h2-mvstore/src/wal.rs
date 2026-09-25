use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use h2_types::{H2Error, H2Result};

/// マップに対する変更（キーバリューの更新または削除）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalChange {
    pub map_name: String,
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>, // None は DELETE
}

/// 1回のトランザクションコミットで記録される WAL レコード
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalRecord {
    pub tx_id: u64,
    pub commit_version: u64,
    pub changes: Vec<WalChange>,
}

pub struct WalManager {
    #[allow(dead_code)]
    path: Option<PathBuf>,
    file: Option<File>,
    sync_on_commit: bool,
}

impl WalManager {
    pub fn open<P: AsRef<Path>>(db_path: P, sync_on_commit: bool) -> H2Result<Self> {
        let wal_path = db_path.as_ref().with_extension("wal");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&wal_path)?;

        Ok(Self {
            path: Some(wal_path),
            file: Some(file),
            sync_on_commit,
        })
    }

    pub fn open_in_memory() -> Self {
        Self {
            path: None,
            file: None,
            sync_on_commit: false,
        }
    }

    pub fn is_in_memory(&self) -> bool {
        self.file.is_none()
    }

    /// WAL レコードをファイル末尾に追記
    pub fn append(&mut self, record: &WalRecord) -> H2Result<()> {
        let Some(file) = &mut self.file else {
            return Ok(());
        };

        let payload_bytes = bincode::serialize(record)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        let len = payload_bytes.len() as u32;

        let mut hasher = Hasher::new();
        hasher.update(&payload_bytes);
        let checksum = hasher.finalize();

        let mut buf = Vec::with_capacity(8 + payload_bytes.len());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&checksum.to_le_bytes());
        buf.extend_from_slice(&payload_bytes);

        file.seek(SeekFrom::End(0))?;
        file.write_all(&buf)?;

        if self.sync_on_commit {
            file.sync_data()?;
        }

        Ok(())
    }

    /// WAL 内の全コミットレコードを読み出す（クラッシュリカバリ用）
    pub fn read_all_records(&mut self) -> H2Result<Vec<WalRecord>> {
        let Some(file) = &mut self.file else {
            return Ok(Vec::new());
        };

        file.seek(SeekFrom::Start(0))?;
        let mut records = Vec::new();
        let mut header = [0u8; 8];

        loop {
            match file.read_exact(&mut header) {
                Ok(_) => {
                    let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
                    let expected_crc = u32::from_le_bytes(header[4..8].try_into().unwrap());

                    let mut payload = vec![0u8; len];
                    if let Err(_) = file.read_exact(&mut payload) {
                        // 途中でファイルが切れている場合はそこまでのコミットを採用（クラッシュセーフ）
                        break;
                    }

                    let mut hasher = Hasher::new();
                    hasher.update(&payload);
                    if hasher.finalize() != expected_crc {
                        // CRC 不一致時は末尾破損として停止
                        break;
                    }

                    if let Ok(record) = bincode::deserialize::<WalRecord>(&payload) {
                        records.push(record);
                    } else {
                        break;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    break;
                }
                Err(e) => return Err(H2Error::Io(e)),
            }
        }

        Ok(records)
    }

    /// チェックポイント完了時に WAL をクリア（切り捨て）
    pub fn clear(&mut self) -> H2Result<()> {
        if let Some(file) = &mut self.file {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            if self.sync_on_commit {
                file.sync_data()?;
            }
        }
        Ok(())
    }

    pub fn set_sync_on_commit(&mut self, sync: bool) {
        self.sync_on_commit = sync;
    }

    pub fn sync(&mut self) -> H2Result<()> {
        if let Some(file) = &mut self.file {
            file.sync_all()?;
        }
        Ok(())
    }
}
