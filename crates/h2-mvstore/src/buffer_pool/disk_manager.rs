use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use parking_lot::Mutex;

use super::slotted_page::PAGE_SIZE;

/// ディスク I/O 管理
pub struct DiskManager {
    file: Mutex<Option<File>>,
    memory_pages: Mutex<HashMap<u32, [u8; PAGE_SIZE]>>,
    num_pages: AtomicU32,
    free_pages: Mutex<Vec<u32>>,
}

impl DiskManager {
    /// メモリ専用（ファイルなし）の DiskManager
    pub fn new_in_memory() -> Self {
        Self {
            file: Mutex::new(None),
            memory_pages: Mutex::new(HashMap::new()),
            num_pages: AtomicU32::new(0),
            free_pages: Mutex::new(Vec::new()),
        }
    }

    /// ファイル永続化用の DiskManager
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;

        let metadata = file.metadata()?;
        let file_len = metadata.len();
        let num_pages = (file_len / PAGE_SIZE as u64) as u32;

        Ok(Self {
            file: Mutex::new(Some(file)),
            memory_pages: Mutex::new(HashMap::new()),
            num_pages: AtomicU32::new(num_pages),
            free_pages: Mutex::new(Vec::new()),
        })
    }

    /// ページを読み出し
    pub fn read_page(&self, page_id: u32, buf: &mut [u8; PAGE_SIZE]) -> io::Result<()> {
        let mut file_guard = self.file.lock();
        if let Some(ref mut file) = *file_guard {
            let offset = (page_id as u64) * (PAGE_SIZE as u64);
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(buf)?;
            Ok(())
        } else {
            let mem = self.memory_pages.lock();
            if let Some(data) = mem.get(&page_id) {
                buf.copy_from_slice(data);
            } else {
                buf.fill(0);
            }
            Ok(())
        }
    }

    /// ページを書き出し
    pub fn write_page(&self, page_id: u32, buf: &[u8; PAGE_SIZE]) -> io::Result<()> {
        let mut file_guard = self.file.lock();
        if let Some(ref mut file) = *file_guard {
            let offset = (page_id as u64) * (PAGE_SIZE as u64);
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(buf)?;
            file.flush()?;
        } else {
            let mut mem = self.memory_pages.lock();
            mem.insert(page_id, *buf);
        }

        let current = self.num_pages.load(Ordering::Relaxed);
        if page_id >= current {
            self.num_pages.store(page_id + 1, Ordering::Relaxed);
        }
        Ok(())
    }

    /// 新規ページIDを発行（空きページがあれば優先再利用）
    pub fn allocate_page(&self) -> u32 {
        let mut free_guard = self.free_pages.lock();
        if let Some(reused_page_id) = free_guard.pop() {
            return reused_page_id;
        }
        self.num_pages.fetch_add(1, Ordering::SeqCst)
    }

    /// デッドタプル回収などで不要になったページを再利用可能リストに返却
    pub fn deallocate_page(&self, page_id: u32) {
        let mut free_guard = self.free_pages.lock();
        if !free_guard.contains(&page_id) {
            free_guard.push(page_id);
        }
    }

    /// 現在再利用可能な空きページ数を取得
    pub fn free_page_count(&self) -> usize {
        self.free_pages.lock().len()
    }

    pub fn num_pages(&self) -> u32 {
        self.num_pages.load(Ordering::Relaxed)
    }
}
