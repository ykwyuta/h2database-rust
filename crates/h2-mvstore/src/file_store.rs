use crate::chunk::ChunkPayload;
use crate::delta::DeltaPayload;
use crc32fast::Hasher;
use h2_types::{H2Error, H2Result};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const HEADER_MAGIC: &[u8; 4] = b"H2RS";
const HEADER_SIZE: u64 = 4096;
const SHADOW_HEADER_OFFSET: u64 = 2048;
const HEADER_RECORD_SIZE: usize = 28;

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

fn write_header_slots(file: &mut File, header: &Header, sync: bool) -> H2Result<()> {
    let bytes = header.serialize();
    // 新しい参照先をシャドウへ先に確定し、その後で従来のヘッダを更新する。
    file.seek(SeekFrom::Start(SHADOW_HEADER_OFFSET))?;
    file.write_all(&bytes[..HEADER_RECORD_SIZE])?;
    if sync {
        file.sync_all()?;
    }
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&bytes[..HEADER_RECORD_SIZE])?;
    if sync {
        file.sync_all()?;
    }
    Ok(())
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
        // 再書き込み中に停止し、旧ファイルを backup へ移した直後なら復元する。
        let backup_path = path_buf.with_extension("compact_bak");
        if !path_buf.exists() && backup_path.exists() {
            std::fs::rename(&backup_path, &path_buf)?;
        }
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
            let primary = Header::deserialize(&buf);
            let mut shadow_buf = [0u8; 4096];
            shadow_buf[..HEADER_RECORD_SIZE].copy_from_slice(
                &buf[SHADOW_HEADER_OFFSET as usize..SHADOW_HEADER_OFFSET as usize + HEADER_RECORD_SIZE],
            );
            let shadow = Header::deserialize(&shadow_buf);
            match (primary, shadow) {
                (Ok(first), Ok(second)) if second.version > first.version => second,
                (Ok(first), _) => first,
                (Err(_), Ok(second)) => second,
                (Err(error), Err(_)) => return Err(error),
            }
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
        if self.sync_on_commit {
            file.sync_all()?;
        }

        // データが耐久化してからヘッダの参照先を切り替える。
        let new_header = Header {
            version: chunk.meta.version,
            last_chunk_offset: end_offset,
            last_chunk_length: chunk_len,
        };

        write_header_slots(file, &new_header, self.sync_on_commit)?;
        self.header = new_header;

        Ok(())
    }

    /// 既存の全量チャンクを基点に差分を追記する。
    pub fn append_delta_and_commit(&mut self, mut delta: DeltaPayload) -> H2Result<()> {
        let Some(file) = &mut self.file else {
            self.header.version = delta.version;
            return Ok(());
        };
        delta.previous_offset = self.header.last_chunk_offset;
        delta.previous_length = self.header.last_chunk_length;
        let bytes = delta.serialize()?;
        let offset = file.seek(SeekFrom::End(0))?;
        file.write_all(&bytes)?;
        if self.sync_on_commit {
            file.sync_all()?;
        }

        let new_header = Header {
            version: delta.version,
            last_chunk_offset: offset,
            last_chunk_length: bytes.len() as u32,
        };
        write_header_slots(file, &new_header, self.sync_on_commit)?;
        self.header = new_header;
        Ok(())
    }

    /// 最新ヘッダから全量チャンクまで遡り、差分を古い順に返す。
    pub fn read_checkpoint_chain(&mut self) -> H2Result<(Option<ChunkPayload>, Vec<DeltaPayload>)> {
        let Some(file) = &mut self.file else {
            return Ok((None, Vec::new()));
        };
        let mut offset = self.header.last_chunk_offset;
        let mut length = self.header.last_chunk_length;
        let mut deltas = Vec::new();
        let mut visited = HashSet::new();
        let file_len = file.metadata()?.len();
        while length != 0 {
            if !visited.insert(offset)
                || offset < HEADER_SIZE
                || offset
                    .checked_add(length as u64)
                    .is_none_or(|end| end > file_len)
            {
                return Err(H2Error::Corrupted(
                    "Invalid checkpoint chain offset".to_string(),
                ));
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0u8; length as usize];
            file.read_exact(&mut bytes)?;
            if DeltaPayload::is_delta(&bytes) {
                let delta = DeltaPayload::deserialize(&bytes)?;
                offset = delta.previous_offset;
                length = delta.previous_length;
                deltas.push(delta);
            } else {
                let base = ChunkPayload::deserialize(&bytes)?;
                deltas.reverse();
                return Ok((Some(base), deltas));
            }
        }
        deltas.reverse();
        Ok((None, deltas))
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
        write_header_slots(&mut temp_file, &final_header, true)?;

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
            return Err(H2Error::Storage(
                "Failed to replace storage file during compaction".to_string(),
            ));
        }

        // 置換後のファイルを再オープン
        let reopened = OpenOptions::new().read(true).write(true).open(path)?;

        self.file = Some(reopened);
        self.header = final_header;

        Ok(())
    }
}
