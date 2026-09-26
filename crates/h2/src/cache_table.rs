use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use crate::Connection;

/// バックグラウンドで定期的にインメモリキャッシュテーブルの差分 Write-Behind および TTL 失効回収を行うワーカー
pub struct CacheWriteBehindCleaner {
    stop_signal: Arc<AtomicBool>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl CacheWriteBehindCleaner {
    /// 定期 Write-Behind / クリーナーを起動
    pub fn start(conn: Connection, interval: Duration) -> Self {
        let stop_signal = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_signal);

        let handle = std::thread::Builder::new()
            .name("h2-cache-write-behind-cleaner".to_string())
            .spawn(move || {
                while !stop_clone.load(Ordering::SeqCst) {
                    std::thread::sleep(interval.min(Duration::from_millis(50)));
                    if stop_clone.load(Ordering::SeqCst) {
                        break;
                    }

                    let tables = conn.engine().catalog().all_tables();
                    for tbl in tables {
                        if tbl.is_cache {
                            if let Ok(tx) = conn.transaction() {
                                if let Some(inner) = tx.inner_tx() {
                                    if tbl.write_back_table.is_some() {
                                        let _ = conn.engine().flush_cache_write_back(inner, &tbl.name);
                                    } else {
                                        let _ = conn.engine().purge_cache_expired(inner, &tbl.name);
                                    }
                                    let _ = tx.commit();
                                }
                            }
                        }
                    }
                }
            })
            .expect("Failed to spawn CacheWriteBehindCleaner thread");

        Self {
            stop_signal,
            thread_handle: Some(handle),
        }
    }

    /// クリーナーを停止
    pub fn stop(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CacheWriteBehindCleaner {
    fn drop(&mut self) {
        self.stop();
    }
}
