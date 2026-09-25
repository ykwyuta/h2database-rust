use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use crc32fast::Hasher;
use h2_types::{H2Error, H2Result};
use crate::chunk::ChunkPayload;

const HEADER_MAGIC: &[u8; 4] = b"H2RS";
const HEADER_SIZE: u64 = 4096;

#[derive(Debug, Clone)]
pub struct Header {
    pub version: u64,
    pub last_chunk_offset: u64,
    pub last_chunk_length: u32,
}

impl Header {
    pub fn serialize(&self) -> [u8; 4096] {
        let mut buf = [0u8; 4096];
        buf[0..4].copy_from_slice(HEADER_MAGIC);
        buf[4..12].copy_from_slice(&self.version.to_le_bytes());
        buf[12..20].copy_from_slice(&self.last_chunk_offset.to_le_bytes());
        buf[20..24].copy_from_slice(&self.last_chunk_length.to_le_bytes());

        let mut hasher = Hasher::new();
        hasher.update(&buf[0..24]);
        let checksum = hasher.finalize();
        buf[24..28].copy_from_slice(&checksum.to_le_bytes());

        buf
    }

    pub fn deserialize(buf: &[u8; 4096]) -> H2Result<Self> {
        if &buf[0..4] != HEADER_MAGIC {
            return Err(H2Error::Corrupted("Invalid header magic bytes".to_string()));
        }

        let mut hasher = Hasher::new();
        hasher.update(&buf[0..24]);
        let checksum = hasher.finalize();
        let expected_checksum = u32::from_le_bytes(buf[24..28].try_into().unwrap());

        if checksum != expected_checksum {
            return Err(H2Error::Corrupted("Header checksum mismatch".to_string()));
        }

        let version = u64::from_le_bytes(buf[4..12].try_into().unwrap());
        let last_chunk_offset = u64::from_le_bytes(buf[12..20].try_into().unwrap());
        let last_chunk_length = u32::from_le_bytes(buf[20..24].try_into().unwrap());

        Ok(Self {
            version,
            last_chunk_offset,
            last_chunk_length,
        })
    }
}

pub struct FileStore {
    path: Option<PathBuf>,
    file: Option<File>,
    header: Header,
    sync_on_commit: bool,
}

impl FileStore {
    pub fn open<P: AsRef<Path>>(path: P) -> H2Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let is_new = !path_buf.exists();

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path_buf)?;

        let header = if is_new || file.metadata()?.len() < HEADER_SIZE {
            let initial_header = Header {
                version: 0,
                last_chunk_offset: 0,
                last_chunk_length: 0,
            };
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&initial_header.serialize())?;
            file.sync_all()?;
            initial_header
        } else {
            let mut buf = [0u8; 4096];
            file.seek(SeekFrom::Start(0))?;
            file.read_exact(&mut buf)?;
            Header::deserialize(&buf)?
        };

        let sync_on_commit = std::env::var("H2_SYNC_COMMIT")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);

        Ok(Self {
            path: Some(path_buf),
            file: Some(file),
            header,
            sync_on_commit,
        })
    }

    pub fn open_in_memory() -> Self {
        Self {
            path: None,
            file: None,
            header: Header {
                version: 0,
                last_chunk_offset: 0,
                last_chunk_length: 0,
            },
            sync_on_commit: false,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_in_memory(&self) -> bool {
        self.file.is_none()
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn sync_on_commit(&self) -> bool {
        self.sync_on_commit
    }

    /// 最新チャンクをファイル末尾に追記し、ヘッダを更新してコミット
    pub fn append_and_commit(&mut self, chunk: &ChunkPayload) -> H2Result<()> {
        let Some(file) = &mut self.file else {
            // インメモリ時はファイルI/Oなしでバージョンのみ進める
            self.header.version = chunk.meta.version;
            return Ok(());
        };

        let chunk_bytes = chunk.serialize()?;
        let chunk_len = chunk_bytes.len() as u32;

        let end_offset = file.seek(SeekFrom::End(0))?;
        file.write_all(&chunk_bytes)?;

        // ヘッダの更新
        self.header.version = chunk.meta.version;
        self.header.last_chunk_offset = end_offset;
        self.header.last_chunk_length = chunk_len;

        file.seek(SeekFrom::Start(0))?;
        file.write_all(&self.header.serialize())?;
        if self.sync_on_commit {
            file.sync_all()?;
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

    /// 最新のチャンクを読み出す
    pub fn read_last_chunk(&mut self) -> H2Result<Option<ChunkPayload>> {
        let Some(file) = &mut self.file else {
            return Ok(None);
        };

        if self.header.last_chunk_offset == 0 && self.header.last_chunk_length == 0 {
            return Ok(None);
        }

        file.seek(SeekFrom::Start(self.header.last_chunk_offset))?;
        let mut buffer = vec![0u8; self.header.last_chunk_length as usize];
        file.read_exact(&mut buffer)?;

        let payload = ChunkPayload::deserialize(&buffer)?;
        Ok(Some(payload))
    }

    /// 生存データのみを新しいファイルに書き出して古い死にチャンクを完全に回収 (Vacuum)
    pub fn compact_and_rewrite(&mut self, payload: &ChunkPayload) -> H2Result<()> {
        let Some(path) = &self.path else {
            // インメモリ時は何もしない
            return Ok(());
        };

        let temp_path = path.with_extension("compact_tmp");

        // 一時ファイルの作成と初期化
        let mut temp_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)?;

        // 一時ファイルにヘッダ初期化
        let dummy_header = Header {
            version: payload.meta.version,
            last_chunk_offset: 0,
            last_chunk_length: 0,
        };
        temp_file.seek(SeekFrom::Start(0))?;
        temp_file.write_all(&dummy_header.serialize())?;

        // チャンクデータの書き出し
        let chunk_bytes = payload.serialize()?;
        let chunk_len = chunk_bytes.len() as u32;
        let offset = temp_file.seek(SeekFrom::End(0))?;
        temp_file.write_all(&chunk_bytes)?;
        temp_file.sync_data()?;

        // ヘッダの確定
        let final_header = Header {
            version: payload.meta.version,
            last_chunk_offset: offset,
            last_chunk_length: chunk_len,
        };
        temp_file.seek(SeekFrom::Start(0))?;
        temp_file.write_all(&final_header.serialize())?;
        temp_file.sync_all()?;

        // ハンドルを閉じて置換準備
        drop(temp_file);
        self.file = None;

        // Windowsでも安全なアトミックファイル置換
        let backup_path = path.with_extension("compact_bak");
        if backup_path.exists() {
            let _ = std::fs::remove_file(&backup_path);
        }

        // 既存ファイルをバックアップへ移動し、新ファイルを配置
        let mut replaced = false;
        for _ in 0..10 {
            if std::fs::rename(path, &backup_path).is_ok() {
                if std::fs::rename(&temp_path, path).is_ok() {
                    let _ = std::fs::remove_file(&backup_path);
                    replaced = true;
                    break;
                } else {
                    // ロールバック
                    let _ = std::fs::rename(&backup_path, path);
                }
            } else if std::fs::rename(&temp_path, path).is_ok() {
                // 直接置換に成功した場合
                replaced = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        if !replaced {
            return Err(H2Error::Storage("Failed to replace storage file during compaction".to_string()));
        }

        // 置換後のファイルを再オープン
        let reopened = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;

        self.file = Some(reopened);
        self.header = final_header;

        Ok(())
    }
}
