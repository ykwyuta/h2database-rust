use crc32fast::Hasher;
use h2_types::{H2Error, H2Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

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
    #[serde(default)]
    pub timestamp_nanos: i64,
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
        self.append_unsynced(record)?;
        if self.sync_on_commit {
            let sync_start = h2_types::query_metrics::enabled().then(Instant::now);
            let sync_result = self.sync_for_commit();
            if let Some(start) = sync_start {
                h2_types::query_metrics::record_wal_sync(start.elapsed());
            }
            sync_result?;
        }
        Ok(())
    }

    pub fn append_unsynced(&mut self, record: &WalRecord) -> H2Result<()> {
        let Some(file) = &mut self.file else {
            return Ok(());
        };

        let payload_bytes =
            bincode::serialize(record).map_err(|e| H2Error::Serialization(e.to_string()))?;
        let len = payload_bytes.len() as u32;

        let mut hasher = Hasher::new();
        hasher.update(&payload_bytes);
        let checksum = hasher.finalize();

        let mut buf = Vec::with_capacity(8 + payload_bytes.len());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&checksum.to_le_bytes());
        buf.extend_from_slice(&payload_bytes);

        let write_start = h2_types::query_metrics::enabled().then(Instant::now);
        let write_result = (|| {
            file.seek(SeekFrom::End(0))?;
            file.write_all(&buf)
        })();
        if let Some(start) = write_start {
            h2_types::query_metrics::record_wal_write(start.elapsed());
        }
        write_result?;

        Ok(())
    }

    pub fn sync_for_commit(&mut self) -> H2Result<()> {
        if self.sync_on_commit {
            if let Some(file) = &mut self.file {
                file.sync_data()?;
            }
        }
        Ok(())
    }

    pub fn truncate_to(&mut self, len: u64) -> H2Result<()> {
        if let Some(file) = &mut self.file {
            file.set_len(len)?;
            file.seek(SeekFrom::Start(len))?;
        }
        Ok(())
    }

    pub fn sync_on_commit(&self) -> bool {
        self.sync_on_commit
    }

    /// WAL 内の全コミットレコードを読み出す（クラッシュリカバリ用）
    pub fn read_all_records(&mut self) -> H2Result<Vec<WalRecord>> {
        let Some(file) = &mut self.file else {
            return Ok(Vec::new());
        };

        file.seek(SeekFrom::Start(0))?;
        let mut records = Vec::new();
        let mut header = [0u8; 8];
        let mut valid_end = 0;
        let file_len = file.metadata()?.len();

        loop {
            match file.read_exact(&mut header) {
                Ok(_) => {
                    let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
                    let expected_crc = u32::from_le_bytes(header[4..8].try_into().unwrap());

                    if len as u64 > file_len.saturating_sub(file.stream_position()?) {
                        break;
                    }

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
                        valid_end = file.stream_position()?;
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

        if file.metadata()?.len() > valid_end {
            file.set_len(valid_end)?;
            file.sync_data()?;
        }
        file.seek(SeekFrom::End(0))?;

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

    pub fn len(&self) -> H2Result<u64> {
        match &self.file {
            Some(file) => Ok(file.metadata()?.len()),
            None => Ok(0),
        }
    }
}

/// WAL アーカイブメタデータ
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalArchiveMeta {
    pub min_version: u64,
    pub max_version: u64,
    pub total_records: u64,
    pub segment_files: Vec<String>,
}

/// 継続的 WAL アーカイブマネージャ
pub struct WalArchiver {
    archive_dir: PathBuf,
    current_segment_writer: Option<File>,
    current_segment_name: Option<String>,
    current_segment_bytes: u64,
    segment_size_limit: u64,
    current_segment_id: u64,
    min_version: u64,
    max_version: u64,
    total_records: u64,
}

impl WalArchiver {
    pub const DEFAULT_SEGMENT_SIZE_LIMIT: u64 = 16 * 1024 * 1024; // 16MB

    pub fn new<P: AsRef<Path>>(archive_dir: P) -> H2Result<Self> {
        Self::with_segment_size(archive_dir, Self::DEFAULT_SEGMENT_SIZE_LIMIT)
    }

    pub fn with_segment_size<P: AsRef<Path>>(archive_dir: P, segment_size_limit: u64) -> H2Result<Self> {
        let dir = archive_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;

        // 既存のセグメントファイルをスキャン
        let mut max_segment_id = 0u64;
        let total_records = 0u64;
        let min_version = u64::MAX;
        let max_version = 0u64;

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("wal_") && name.ends_with(".wal") {
                    let num_part = &name[4..name.len() - 4];
                    if let Ok(id) = num_part.parse::<u64>() {
                        if id > max_segment_id {
                            max_segment_id = id;
                        }
                    }
                }
            }
        }

        let next_segment_id = max_segment_id.saturating_add(1);

        Ok(Self {
            archive_dir: dir,
            current_segment_writer: None,
            current_segment_name: None,
            current_segment_bytes: 0,
            segment_size_limit,
            current_segment_id: next_segment_id,
            min_version: if min_version == u64::MAX { 0 } else { min_version },
            max_version,
            total_records,
        })
    }

    pub fn archive_dir(&self) -> &Path {
        &self.archive_dir
    }

    /// WAL レコードをアーカイブへ永続化
    pub fn archive_record(&mut self, record: &WalRecord) -> H2Result<()> {
        let payload_bytes =
            bincode::serialize(record).map_err(|e| H2Error::Serialization(e.to_string()))?;
        let len = payload_bytes.len() as u32;

        let mut hasher = Hasher::new();
        hasher.update(&payload_bytes);
        let checksum = hasher.finalize();

        let total_entry_len = 8 + payload_bytes.len();
        let mut buf = Vec::with_capacity(total_entry_len);
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&checksum.to_le_bytes());
        buf.extend_from_slice(&payload_bytes);

        // セグメントローテーション判定
        if self.current_segment_writer.is_none()
            || self.current_segment_bytes + buf.len() as u64 > self.segment_size_limit
        {
            if let Some(file) = self.current_segment_writer.take() {
                file.sync_data()?;
            }

            let file_name = format!("wal_{:016}.wal", self.current_segment_id);
            let file_path = self.archive_dir.join(&file_name);
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .append(true)
                .open(&file_path)?;

            self.current_segment_writer = Some(file);
            self.current_segment_name = Some(file_name);
            self.current_segment_bytes = 0;
            self.current_segment_id += 1;
        }

        if let Some(file) = &mut self.current_segment_writer {
            file.write_all(&buf)?;
            file.sync_data()?;
            self.current_segment_bytes += buf.len() as u64;
        }

        if self.min_version == 0 || record.commit_version < self.min_version {
            self.min_version = record.commit_version;
        }
        if record.commit_version > self.max_version {
            self.max_version = record.commit_version;
        }
        self.total_records += 1;

        Ok(())
    }

    pub fn flush(&mut self) -> H2Result<()> {
        if let Some(file) = &mut self.current_segment_writer {
            file.sync_all()?;
        }
        Ok(())
    }

    /// アーカイブディレクトリから指定したバージョン以降のレコードを全走査
    pub fn read_archive_records<P: AsRef<Path>>(
        archive_dir: P,
        from_version: u64,
    ) -> H2Result<Vec<WalRecord>> {
        let dir = archive_dir.as_ref();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "wal" {
                        files.push(path);
                    }
                }
            }
        }

        // ファイル名順（wal_0000000000000001.wal 形式なので時系列ソート）
        files.sort();

        let mut records = Vec::new();
        let mut header = [0u8; 8];

        for file_path in files {
            let mut file = match File::open(&file_path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let file_len = file.metadata()?.len();

            loop {
                match file.read_exact(&mut header) {
                    Ok(_) => {
                        let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
                        let expected_crc = u32::from_le_bytes(header[4..8].try_into().unwrap());

                        if len as u64 > file_len.saturating_sub(file.stream_position()?) {
                            break;
                        }

                        let mut payload = vec![0u8; len];
                        if file.read_exact(&mut payload).is_err() {
                            break;
                        }

                        let mut hasher = Hasher::new();
                        hasher.update(&payload);
                        if hasher.finalize() != expected_crc {
                            break;
                        }

                        if let Ok(rec) = bincode::deserialize::<WalRecord>(&payload) {
                            if rec.commit_version > from_version {
                                records.push(rec);
                            }
                        } else {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }

        // commit_version 昇順にソート
        records.sort_by_key(|r| r.commit_version);
        Ok(records)
    }

    /// 既存の WAL ファイルをアーカイブディレクトリに一括退避
    pub fn archive_wal_file<P: AsRef<Path>, A: AsRef<Path>>(
        wal_file_path: P,
        archive_dir: A,
    ) -> H2Result<usize> {
        let wal_path = wal_file_path.as_ref();
        if !wal_path.exists() {
            return Ok(0);
        }

        let mut wal = WalManager::open(wal_path.with_extension(""), false)?;
        let records = wal.read_all_records()?;
        let count = records.len();

        let mut archiver = WalArchiver::new(archive_dir)?;
        for record in &records {
            archiver.archive_record(record)?;
        }
        archiver.flush()?;

        Ok(count)
    }
}

/// PITR 復旧ターゲットの指定
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryTarget {
    /// 指定した時刻（UNIX epoch ナノ秒）直前の状態まで復元
    TimestampNanos(i64),
    /// 指定したコミットバージョン直前の状態まで復元
    Version(u64),
    /// 指定したトランザクション ID 完了時点まで復元
    TransactionId(u64),
    /// 利用可能な最新のアーカイブ WAL まで完全にロールフォワード
    Latest,
}

impl RecoveryTarget {
    pub fn timestamp(t: std::time::SystemTime) -> Self {
        let nanos = t
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        Self::TimestampNanos(nanos)
    }

    pub fn timestamp_nanos(nanos: i64) -> Self {
        Self::TimestampNanos(nanos)
    }

    pub fn version(v: u64) -> Self {
        Self::Version(v)
    }

    pub fn transaction_id(tx_id: u64) -> Self {
        Self::TransactionId(tx_id)
    }

    pub fn latest() -> Self {
        Self::Latest
    }
}

/// PITR 復元の実行結果レポート
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub base_snapshot_version: u64,
    pub final_recovered_version: u64,
    pub records_replayed: usize,
    pub target_reached: bool,
    pub is_replica_backup: bool,
}
