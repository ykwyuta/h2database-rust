pub mod clock_replacer;
pub mod disk_manager;
pub mod slotted_page;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use parking_lot::RwLock;

pub use clock_replacer::ClockReplacer;
pub use disk_manager::DiskManager;
pub use slotted_page::{PAGE_SIZE, SlottedPage};

use h2_types::{H2Error, H2Result};

/// メモリ上の 8KB ページフレーム
pub struct PageFrame {
    pub data: [u8; PAGE_SIZE],
    pub page_id: u32,
    pub pin_count: AtomicU32,
    pub is_dirty: AtomicBool,
}

impl PageFrame {
    pub fn new() -> Self {
        Self {
            data: [0u8; PAGE_SIZE],
            page_id: u32::MAX,
            pin_count: AtomicU32::new(0),
            is_dirty: AtomicBool::new(false),
        }
    }
}

/// Buffer Pool Manager (Phase 2 ストレージ層 Out-of-Core 刷新の中核)
pub struct BufferPoolManager {
    disk_manager: Arc<DiskManager>,
    replacer: Arc<ClockReplacer>,
    frames: Vec<RwLock<PageFrame>>,
    page_table: RwLock<HashMap<u32, usize>>, // page_id -> frame_id
    pool_size: usize,
}

impl BufferPoolManager {
    pub fn new(disk_manager: Arc<DiskManager>, pool_size: usize) -> Self {
        let mut frames = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            frames.push(RwLock::new(PageFrame::new()));
        }
        let replacer = Arc::new(ClockReplacer::new(pool_size));
        // 初期状態では全フレームが空き（replacerに追加）
        for frame_id in 0..pool_size {
            replacer.unpin(frame_id);
        }

        Self {
            disk_manager,
            replacer,
            frames,
            page_table: RwLock::new(HashMap::new()),
            pool_size,
        }
    }

    /// ページIDを指定してフレームを取得（pin_count を +1）
    pub fn fetch_page(&self, page_id: u32) -> H2Result<usize> {
        // 1. キャッシュヒット判定
        {
            let page_table = self.page_table.read();
            if let Some(&frame_id) = page_table.get(&page_id) {
                let frame = self.frames[frame_id].read();
                frame.pin_count.fetch_add(1, Ordering::SeqCst);
                self.replacer.pin(frame_id);
                return Ok(frame_id);
            }
        }

        // 2. キャッシュミス: 空きフレームまたは victim フレームを選定
        let victim_frame_id = self.replacer.victim().ok_or_else(|| {
            H2Error::Storage("All buffer pool frames are currently pinned; out of memory".to_string())
        })?;

        // 3. 古いページのディスク退避（dirty の場合）
        {
            let mut frame = self.frames[victim_frame_id].write();
            if frame.is_dirty.load(Ordering::SeqCst) && frame.page_id != u32::MAX {
                self.disk_manager
                    .write_page(frame.page_id, &frame.data)
                    .map_err(|e| H2Error::Storage(format!("Disk write error during page flush: {}", e)))?;
                frame.is_dirty.store(false, Ordering::SeqCst);
            }

            // 旧マッピングの削除
            let mut page_table = self.page_table.write();
            if frame.page_id != u32::MAX {
                page_table.remove(&frame.page_id);
            }

            // 新ページの読み込み
            self.disk_manager
                .read_page(page_id, &mut frame.data)
                .map_err(|e| H2Error::Storage(format!("Disk read error during page fetch: {}", e)))?;

            frame.page_id = page_id;
            frame.pin_count.store(1, Ordering::SeqCst);
            frame.is_dirty.store(false, Ordering::SeqCst);

            page_table.insert(page_id, victim_frame_id);
        }

        self.replacer.pin(victim_frame_id);
        Ok(victim_frame_id)
    }

    /// 新規ページを作成してバッファプールに確保
    pub fn new_page(&self, page_type: u8) -> H2Result<(u32, usize)> {
        let victim_frame_id = self.replacer.victim().ok_or_else(|| {
            H2Error::Storage("All buffer pool frames are pinned; cannot allocate new page".to_string())
        })?;

        let new_page_id = self.disk_manager.allocate_page();

        {
            let mut frame = self.frames[victim_frame_id].write();
            if frame.is_dirty.load(Ordering::SeqCst) && frame.page_id != u32::MAX {
                self.disk_manager
                    .write_page(frame.page_id, &frame.data)
                    .map_err(|e| H2Error::Storage(format!("Disk write error: {}", e)))?;
                frame.is_dirty.store(false, Ordering::SeqCst);
            }

            let mut page_table = self.page_table.write();
            if frame.page_id != u32::MAX {
                page_table.remove(&frame.page_id);
            }

            SlottedPage::init(&mut frame.data, new_page_id, page_type);
            frame.page_id = new_page_id;
            frame.pin_count.store(1, Ordering::SeqCst);
            frame.is_dirty.store(true, Ordering::SeqCst); // 新規ページはダーティ

            page_table.insert(new_page_id, victim_frame_id);
        }

        self.replacer.pin(victim_frame_id);
        Ok((new_page_id, victim_frame_id))
    }

    /// ページのアンピン（ピンカウント -1）
    pub fn unpin_page(&self, page_id: u32, is_dirty: bool) -> H2Result<()> {
        let page_table = self.page_table.read();
        if let Some(&frame_id) = page_table.get(&page_id) {
            let frame = self.frames[frame_id].read();
            if is_dirty {
                frame.is_dirty.store(true, Ordering::SeqCst);
            }
            let prev = frame.pin_count.fetch_sub(1, Ordering::SeqCst);
            if prev == 1 {
                // pin_count が 0 になったので置換可能に登録
                self.replacer.unpin(frame_id);
            }
            Ok(())
        } else {
            Err(H2Error::Storage(format!("Page {} not found in buffer pool", page_id)))
        }
    }

    /// 指定ページをディスクにフラッシュ
    pub fn flush_page(&self, page_id: u32) -> H2Result<()> {
        let page_table = self.page_table.read();
        if let Some(&frame_id) = page_table.get(&page_id) {
            let mut frame = self.frames[frame_id].write();
            if frame.is_dirty.load(Ordering::SeqCst) {
                SlottedPage::compute_and_write_checksum(&mut frame.data);
                self.disk_manager
                    .write_page(page_id, &frame.data)
                    .map_err(|e| H2Error::Storage(format!("Flush error: {}", e)))?;
                frame.is_dirty.store(false, Ordering::SeqCst);
            }
        }
        Ok(())
    }

    /// 全ダーティページをディスクにフラッシュ
    pub fn flush_all(&self) -> H2Result<()> {
        let page_table = self.page_table.read();
        for (&page_id, &frame_id) in page_table.iter() {
            let mut frame = self.frames[frame_id].write();
            if frame.is_dirty.load(Ordering::SeqCst) {
                SlottedPage::compute_and_write_checksum(&mut frame.data);
                self.disk_manager
                    .write_page(page_id, &frame.data)
                    .map_err(|e| H2Error::Storage(format!("Flush all error: {}", e)))?;
                frame.is_dirty.store(false, Ordering::SeqCst);
            }
        }
        Ok(())
    }

    /// フレームのデータへのアクセス
    pub fn get_frame(&self, frame_id: usize) -> &RwLock<PageFrame> {
        &self.frames[frame_id]
    }

    /// ページを解放し、空きページ再利用リストへ登録
    pub fn free_page(&self, page_id: u32) -> H2Result<()> {
        let frame_id = self.fetch_page(page_id)?;
        {
            let mut frame = self.frames[frame_id].write();
            SlottedPage::mark_as_free(&mut frame.data);
            frame.is_dirty.store(true, Ordering::SeqCst);
        }
        self.unpin_page(page_id, true)?;
        self.disk_manager.deallocate_page(page_id);
        Ok(())
    }

    /// ページのバキューム（全タプルがデッドの場合は空きページ化して再利用リストへ登録、生存タプルがある場合はデフラグ）
    /// ページが解放された場合は Ok(true)、生存タプルがありデフラグされた場合は Ok(false) を返す
    pub fn vacuum_page(&self, page_id: u32) -> H2Result<bool> {
        let frame_id = self.fetch_page(page_id)?;
        let is_dead = {
            let frame = self.frames[frame_id].read();
            SlottedPage::is_all_dead(&frame.data)
        };

        if is_dead {
            {
                let mut frame = self.frames[frame_id].write();
                SlottedPage::mark_as_free(&mut frame.data);
                frame.is_dirty.store(true, Ordering::SeqCst);
            }
            self.unpin_page(page_id, true)?;
            self.disk_manager.deallocate_page(page_id);
            Ok(true)
        } else {
            {
                let mut frame = self.frames[frame_id].write();
                SlottedPage::defragment(&mut frame.data);
                frame.is_dirty.store(true, Ordering::SeqCst);
            }
            self.unpin_page(page_id, true)?;
            Ok(false)
        }
    }

    /// 現在再利用可能な空きページ数を取得
    pub fn free_page_count(&self) -> usize {
        self.disk_manager.free_page_count()
    }

    pub fn pool_size(&self) -> usize {
        self.pool_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_pool_manager_crud() {
        let disk = Arc::new(DiskManager::new_in_memory());
        let bpm = BufferPoolManager::new(disk, 4);

        // 新規ページ作成
        let (page0, frame0) = bpm.new_page(0).unwrap();
        assert_eq!(page0, 0);

        // データ書き込み
        {
            let mut f = bpm.get_frame(frame0).write();
            SlottedPage::insert_tuple(&mut f.data, b"buffered record").unwrap();
        }

        // アンピン
        bpm.unpin_page(page0, true).unwrap();

        // 別ページの割り当て
        let (page1, _) = bpm.new_page(0).unwrap();
        let (page2, _) = bpm.new_page(0).unwrap();
        let (page3, _) = bpm.new_page(0).unwrap();
        bpm.unpin_page(page1, false).unwrap();
        bpm.unpin_page(page2, false).unwrap();
        bpm.unpin_page(page3, false).unwrap();

        // ページ0の再取得
        let frame0_again = bpm.fetch_page(page0).unwrap();
        {
            let f = bpm.get_frame(frame0_again).read();
            assert_eq!(SlottedPage::get_tuple(&f.data, 0), Some(&b"buffered record"[..]));
        }
        bpm.unpin_page(page0, false).unwrap();
    }

    #[test]
    fn test_dead_tuple_page_vacuum_and_reuse() {
        let disk = Arc::new(DiskManager::new_in_memory());
        let bpm = BufferPoolManager::new(disk, 4);

        // ページ0作成 & タプル挿入
        let (page0, frame0) = bpm.new_page(0).unwrap();
        assert_eq!(page0, 0);
        {
            let mut f = bpm.get_frame(frame0).write();
            let s0 = SlottedPage::insert_tuple(&mut f.data, b"dead tuple").unwrap();
            assert_eq!(s0, 0);
        }
        bpm.unpin_page(page0, true).unwrap();

        // タプルを論理削除
        {
            let frame0 = bpm.fetch_page(page0).unwrap();
            {
                let mut f = bpm.get_frame(frame0).write();
                SlottedPage::delete_tuple(&mut f.data, 0);
                assert!(SlottedPage::is_all_dead(&f.data));
            }
            bpm.unpin_page(page0, true).unwrap();
        }

        // vacuum_page を実行 -> 全タプルが死んでいるためページが解放（Free Page Listへ）
        let reclaimed = bpm.vacuum_page(page0).unwrap();
        assert!(reclaimed);
        assert_eq!(bpm.free_page_count(), 1);

        // 次の new_page の割り当てで、解放された page0 が再利用される！
        let (reused_page, _) = bpm.new_page(0).unwrap();
        assert_eq!(reused_page, 0);
        assert_eq!(bpm.free_page_count(), 0);
    }
}
