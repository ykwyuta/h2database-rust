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

        Ok(Self {
            path: Some(path_buf),
            file: Some(file),
            header,
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
        file.sync_data()?;

        // ヘッダの更新
        self.header.version = chunk.meta.version;
        self.header.last_chunk_offset = end_offset;
        self.header.last_chunk_length = chunk_len;

        file.seek(SeekFrom::Start(0))?;
        file.write_all(&self.header.serialize())?;
        file.sync_all()?;

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
}
