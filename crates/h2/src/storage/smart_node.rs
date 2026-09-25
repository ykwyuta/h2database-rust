use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use parking_lot::RwLock;

use h2_mvstore::MVStore;
use h2_types::{FencingToken, H2Error, H2Result, LogOpType, LogRecord, Lsn, PageId};

/// ストレージノードの稼働状態（障害シミュレーション用）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    Online,
    Paused,
    Offline,
}

/// "The Log is the Database" スマートストレージノード
/// 
/// 責務:
/// 1. WAL ログレコードを受信し、高速にアペンド＆fsync（即時 ACK 返却）
/// 2. バックグラウンドで非同期に MVStore B-Tree 構造へマテリアライズ（Redo 適用）
/// 3. オンデマンド Redo 解決（バックグラウンドが追いついていない場合の動的マージ）
/// 4. ゴシップ自己修復（ピアからの欠損ログ同期）
/// 5. Fencing Token によるスプリットブレイン防止
pub struct SmartStorageNode {
    pub node_id: usize,
    pub az: String,
    wal_buffer: Arc<RwLock<Vec<LogRecord>>>,
    flushed_lsn: Arc<AtomicU64>,
    applied_lsn: Arc<AtomicU64>,
    materialized_store: Arc<MVStore>,
    current_fencing_token: Arc<AtomicU64>,
    state: Arc<RwLock<NodeState>>,
    simulated_delay_ms: Arc<AtomicU64>,
}

impl SmartStorageNode {
    pub fn new(node_id: usize, az: impl Into<String>) -> Self {
        let store = Arc::new(MVStore::open_in_memory());
        Self {
            node_id,
            az: az.into(),
            wal_buffer: Arc::new(RwLock::new(Vec::new())),
            flushed_lsn: Arc::new(AtomicU64::new(0)),
            applied_lsn: Arc::new(AtomicU64::new(0)),
            materialized_store: store,
            current_fencing_token: Arc::new(AtomicU64::new(1)),
            state: Arc::new(RwLock::new(NodeState::Online)),
            simulated_delay_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// ノードの最新フラッシュ済み LSN (fsync 完了 LSN)
    pub fn flushed_lsn(&self) -> Lsn {
        self.flushed_lsn.load(Ordering::SeqCst)
    }

    /// ノードの最新マテリアライズ済み LSN (B-Tree 反映完了 LSN)
    pub fn applied_lsn(&self) -> Lsn {
        self.applied_lsn.load(Ordering::SeqCst)
    }

    /// 現在の Fencing Token
    pub fn fencing_token(&self) -> FencingToken {
        FencingToken(self.current_fencing_token.load(Ordering::SeqCst))
    }

    /// 排他書き込みリースの獲得 (Fencing Token 検証)
    pub fn acquire_lease(&self, token: FencingToken) -> H2Result<()> {
        let current = self.current_fencing_token.load(Ordering::SeqCst);
        if token.0 < current {
            return Err(H2Error::Storage(format!(
                "Node {}: Rejected fencing token {} < current {}",
                self.node_id, token.0, current
            )));
        }
        self.current_fencing_token.store(token.0, Ordering::SeqCst);
        Ok(())
    }

    /// 障害シミュレーション用: ノード状態を変更
    pub fn set_state(&self, state: NodeState) {
        *self.state.write() = state;
    }

    pub fn state(&self) -> NodeState {
        *self.state.read()
    }

    /// 障害シミュレーション用: 遅延（ミリ秒）を設定
    pub fn set_simulated_delay(&self, delay_ms: u64) {
        self.simulated_delay_ms.store(delay_ms, Ordering::SeqCst);
    }

    /// ログレコード群の高速アペンド (The Log is the Database)
    /// ダーティページは受信せず、ログレコードのみを受信して即時 ACK
    pub fn append_logs(&self, records: &[LogRecord], token: FencingToken) -> H2Result<Lsn> {
        let current_state = *self.state.read();
        if current_state == NodeState::Offline {
            return Err(H2Error::Storage(format!(
                "Node {} is OFFLINE",
                self.node_id
            )));
        }

        // 遅延シミュレーション (Tail Latency の再現)
        let delay = self.simulated_delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }

        // Fencing Token チェック
        let current_token = self.current_fencing_token.load(Ordering::SeqCst);
        if token.0 < current_token {
            return Err(H2Error::Storage(format!(
                "Node {}: Fencing token violation. Incoming token {} < node token {}",
                self.node_id, token.0, current_token
            )));
        }

        let mut max_lsn = self.flushed_lsn();
        {
            let mut buf = self.wal_buffer.write();
            for rec in records {
                max_lsn = max_lsn.max(rec.lsn);
                buf.push(rec.clone());
            }
        }

        self.flushed_lsn.store(max_lsn, Ordering::SeqCst);
        Ok(max_lsn)
    }

    /// バックグラウンドワーカー: 未適用の WAL ログレコードを内部 B-Tree にマテリアライズ（Redo 適用）
    pub fn materialize_pending(&self) -> usize {
        let current_state = *self.state.read();
        if current_state == NodeState::Offline {
            return 0;
        }

        let applied = self.applied_lsn.load(Ordering::SeqCst);
        let flushed = self.flushed_lsn.load(Ordering::SeqCst);
        if applied >= flushed {
            return 0;
        }

        let logs_to_apply: Vec<LogRecord> = {
            let buf = self.wal_buffer.read();
            buf.iter()
                .filter(|r| r.lsn > applied && r.lsn <= flushed)
                .cloned()
                .collect()
        };

        let count = logs_to_apply.len();
        let mut new_applied = applied;

        for rec in logs_to_apply {
            new_applied = new_applied.max(rec.lsn);
            match rec.op_type {
                LogOpType::Put => {
                    if let Some(val) = rec.value {
                        let map = self.materialized_store.open_map(&rec.map_name);
                        map.put(rec.key, val);
                    }
                }
                LogOpType::Delete => {
                    let map = self.materialized_store.open_map(&rec.map_name);
                    map.remove(&rec.key);
                }
                LogOpType::TruncateMap | LogOpType::DropMap => {
                    let map = self.materialized_store.open_map(&rec.map_name);
                    map.clear();
                }
                LogOpType::Commit => {
                    let _ = self.materialized_store.commit();
                }
                LogOpType::Rollback | LogOpType::CreateMap => {
                    // 何もしないかメタデータのみ更新
                }
            }
        }

        self.applied_lsn.store(new_applied, Ordering::SeqCst);
        count
    }

    /// オンデマンド Redo 解決付きキー取得
    /// バックグラウンドマテリアライザーが未完了であっても、要求 LSN までの未適用ログをオンザフライで合成！
    pub fn get_key(&self, map_name: &str, key: &[u8], read_version: Lsn) -> H2Result<Option<Vec<u8>>> {
        let current_state = *self.state.read();
        if current_state == NodeState::Offline {
            return Err(H2Error::Storage(format!("Node {} is OFFLINE", self.node_id)));
        }

        // 1. まずマテリアライズ済み B-Tree から取得
        let map = self.materialized_store.open_map(map_name);
        let mut base_val = map.get(key);

        // 2. マテリアライズ済み LSN より新しい未適用ログがあれば動的オーバーレイ (On-demand Redo)
        let applied = self.applied_lsn.load(Ordering::SeqCst);
        if read_version > applied {
            let buf = self.wal_buffer.read();
            for rec in buf.iter() {
                if rec.lsn > applied && rec.lsn <= read_version && rec.map_name == map_name && rec.key == key {
                    match rec.op_type {
                        LogOpType::Put => {
                            base_val = rec.value.clone();
                        }
                        LogOpType::Delete => {
                            base_val = None;
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(base_val)
    }

    /// オンデマンド Redo 解決付きレンジスキャン
    pub fn scan_range(
        &self,
        map_name: &str,
        start_key: Option<&[u8]>,
        end_key: Option<&[u8]>,
        read_version: Lsn,
    ) -> H2Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let current_state = *self.state.read();
        if current_state == NodeState::Offline {
            return Err(H2Error::Storage(format!("Node {} is OFFLINE", self.node_id)));
        }

        // 1. ベースの B-Tree から全エントリを取得して BTreeMap に展開
        let map = self.materialized_store.open_map(map_name);
        let mut merged: std::collections::BTreeMap<Vec<u8>, Vec<u8>> = map
            .scan_all()
            .into_iter()
            .map(|e| (e.key, e.value))
            .collect();

        // 2. 未適用の WAL ログをオンデマンドで適用
        let applied = self.applied_lsn.load(Ordering::SeqCst);
        if read_version > applied {
            let buf = self.wal_buffer.read();
            for rec in buf.iter() {
                if rec.lsn > applied && rec.lsn <= read_version && rec.map_name == map_name {
                    match rec.op_type {
                        LogOpType::Put => {
                            if let Some(ref val) = rec.value {
                                merged.insert(rec.key.clone(), val.clone());
                            }
                        }
                        LogOpType::Delete => {
                            merged.remove(&rec.key);
                        }
                        LogOpType::TruncateMap | LogOpType::DropMap => {
                            merged.clear();
                        }
                        _ => {}
                    }
                }
            }
        }

        // 3. レンジフィルタリング
        let filtered = merged
            .into_iter()
            .filter(|(k, _)| {
                if let Some(start) = start_key {
                    if k.as_slice() < start {
                        return false;
                    }
                }
                if let Some(end) = end_key {
                    if k.as_slice() > end {
                        return false;
                    }
                }
                true
            })
            .collect();

        Ok(filtered)
    }

    /// ページ ID からのページバイナリ生成
    pub fn get_page(&self, page_id: PageId, _read_version: Lsn) -> H2Result<Vec<u8>> {
        let current_state = *self.state.read();
        if current_state == NodeState::Offline {
            return Err(H2Error::Storage(format!("Node {} is OFFLINE", self.node_id)));
        }
        let mut buf = Vec::with_capacity(16);
        buf.extend_from_slice(&page_id.segment_id.to_be_bytes());
        buf.extend_from_slice(&page_id.page_no.to_be_bytes());
        Ok(buf)
    }

    /// ゴシップ自己修復 (Peer-to-Peer Gossip Repair)
    /// 健全なピアノードと WAL ログを比較し、自ノードに欠落している LSN 範囲のログを同期して修復
    pub fn gossip_sync_from(&self, peer: &SmartStorageNode) -> H2Result<usize> {
        let current_state = *self.state.read();
        if current_state == NodeState::Offline {
            return Err(H2Error::Storage(format!("Node {} is OFFLINE", self.node_id)));
        }

        let my_flushed = self.flushed_lsn();
        let peer_flushed = peer.flushed_lsn();

        if my_flushed >= peer_flushed {
            return Ok(0); // 欠損なし
        }

        let missing_logs: Vec<LogRecord> = {
            let peer_buf = peer.wal_buffer.read();
            peer_buf
                .iter()
                .filter(|r| r.lsn > my_flushed && r.lsn <= peer_flushed)
                .cloned()
                .collect()
        };

        let count = missing_logs.len();
        if count == 0 {
            return Ok(0);
        }

        let mut max_lsn = my_flushed;
        {
            let mut my_buf = self.wal_buffer.write();
            for rec in missing_logs {
                max_lsn = max_lsn.max(rec.lsn);
                my_buf.push(rec);
            }
        }
        self.flushed_lsn.store(max_lsn, Ordering::SeqCst);

        // 自動マテリアライズも実施
        self.materialize_pending();

        Ok(count)
    }

    /// 保有している全 WAL ログレコードのクローンを取得（検証・インスペクション用）
    pub fn get_all_logs(&self) -> Vec<LogRecord> {
        self.wal_buffer.read().clone()
    }
}
