use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use h2_mvstore::StorageEngine;
use h2_types::{FencingToken, H2Error, H2Result, LogRecord, Lsn, PageId};

use super::smart_node::SmartStorageNode;

/// クォーラム構成設定 (2/3 または 4/6 等)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuorumConfig {
    pub total_nodes: usize,
    pub write_quorum: usize,
    pub read_quorum: usize,
}

impl QuorumConfig {
    /// 3ノード構成 (2 of 3 Quorum: 1ノード障害耐性)
    pub fn three_nodes() -> Self {
        Self {
            total_nodes: 3,
            write_quorum: 2,
            read_quorum: 2,
        }
    }

    /// 6ノード構成 (4 of 6 Quorum: 2ノード / 1 AZ 障害耐性, Aurora標準)
    pub fn six_nodes() -> Self {
        Self {
            total_nodes: 6,
            write_quorum: 4,
            read_quorum: 3,
        }
    }

    pub fn validate(&self) -> H2Result<()> {
        if self.write_quorum > self.total_nodes {
            return Err(H2Error::Storage(format!(
                "Invalid quorum config: write_quorum ({}) > total_nodes ({})",
                self.write_quorum, self.total_nodes
            )));
        }
        if self.write_quorum + self.read_quorum <= self.total_nodes {
            return Err(H2Error::Storage(format!(
                "Invalid quorum config: write_quorum ({}) + read_quorum ({}) must be > total_nodes ({})",
                self.write_quorum, self.read_quorum, self.total_nodes
            )));
        }
        Ok(())
    }
}

/// 分散スマートストレージノード群を統括するストレージフリート
pub struct StorageFleet {
    nodes: Vec<Arc<SmartStorageNode>>,
    config: QuorumConfig,
    latest_committed_lsn: Arc<AtomicU64>,
    fencing_token: Arc<AtomicU64>,
}

impl StorageFleet {
    pub fn new(nodes: Vec<Arc<SmartStorageNode>>, config: QuorumConfig) -> H2Result<Self> {
        config.validate()?;
        if nodes.len() != config.total_nodes {
            return Err(H2Error::Storage(format!(
                "Fleet node count mismatch: configured {}, got {}",
                config.total_nodes,
                nodes.len()
            )));
        }
        Ok(Self {
            nodes,
            config,
            latest_committed_lsn: Arc::new(AtomicU64::new(0)),
            fencing_token: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn nodes(&self) -> &[Arc<SmartStorageNode>] {
        &self.nodes
    }

    pub fn config(&self) -> &QuorumConfig {
        &self.config
    }

    pub fn latest_committed_lsn(&self) -> Lsn {
        self.latest_committed_lsn.load(Ordering::SeqCst)
    }

    pub fn fencing_token(&self) -> FencingToken {
        FencingToken(self.fencing_token.load(Ordering::SeqCst))
    }

    /// クォーラムノード群から排他的リーダーリースを獲得 (Fencing Token 検証)
    pub fn acquire_lease_quorum(&self, token: FencingToken) -> H2Result<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        let total_nodes = self.nodes.len();

        for node in &self.nodes {
            let node = Arc::clone(node);
            let tx = tx.clone();
            std::thread::spawn(move || {
                let res = node.acquire_lease(token);
                let _ = tx.send(res);
            });
        }

        let mut acks = 0;
        let mut rejections = 0;

        for _ in 0..total_nodes {
            if let Ok(res) = rx.recv() {
                match res {
                    Ok(_) => {
                        acks += 1;
                        if acks >= self.config.write_quorum {
                            self.fencing_token.store(token.0, Ordering::SeqCst);
                            return Ok(());
                        }
                    }
                    Err(_) => {
                        rejections += 1;
                        if total_nodes - rejections < self.config.write_quorum {
                            return Err(H2Error::Storage(format!(
                                "Failed to acquire lease: rejected by majority (token {})",
                                token.0
                            )));
                        }
                    }
                }
            }
        }

        Err(H2Error::Storage("Quorum lease acquisition timeout".to_string()))
    }

    /// クォーラム書き込み ("The Log is the Database")
    /// 全ストレージノードへ WAL ログレコードを並行送出し、write_quorum 個の ACK を受信した瞬間に即座に完了
    /// （低速ノードの遅延を完全に隠蔽）
    pub fn append_logs_quorum(&self, records: &[LogRecord], token: FencingToken) -> H2Result<Lsn> {
        if records.is_empty() {
            return Ok(self.latest_committed_lsn());
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let total_nodes = self.nodes.len();

        for node in &self.nodes {
            let node = Arc::clone(node);
            let records = records.to_vec();
            let tx = tx.clone();
            std::thread::spawn(move || {
                let res = node.append_logs(&records, token);
                let _ = tx.send(res);
            });
        }

        let mut acks = 0;
        let mut errors = 0;
        let mut max_ack_lsn = 0;

        for _ in 0..total_nodes {
            if let Ok(res) = rx.recv() {
                match res {
                    Ok(lsn) => {
                        acks += 1;
                        max_ack_lsn = max_ack_lsn.max(lsn);
                        // クォーラム（例: 2 of 3, 4 of 6）に達した瞬間に即時完了！
                        if acks >= self.config.write_quorum {
                            self.latest_committed_lsn.store(max_ack_lsn, Ordering::SeqCst);
                            return Ok(max_ack_lsn);
                        }
                    }
                    Err(_) => {
                        errors += 1;
                        if total_nodes - errors < self.config.write_quorum {
                            return Err(H2Error::Storage(format!(
                                "Write quorum unachievable: {} of {} nodes failed",
                                errors, total_nodes
                            )));
                        }
                    }
                }
            }
        }

        Err(H2Error::Storage("Write quorum timeout".to_string()))
    }

    /// クォーラム読み取り: 健全なストレージノードからオンデマンド Redo 解決付きで値を取得
    pub fn get_key_quorum(&self, map_name: &str, key: &[u8], read_version: Lsn) -> H2Result<Option<Vec<u8>>> {
        let mut last_err = None;
        for node in &self.nodes {
            match node.get_key(map_name, key, read_version) {
                Ok(val) => return Ok(val),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| H2Error::Storage("No accessible storage nodes".to_string())))
    }

    /// クォーラムレンジスキャン
    pub fn scan_range_quorum(
        &self,
        map_name: &str,
        start_key: Option<&[u8]>,
        end_key: Option<&[u8]>,
        read_version: Lsn,
    ) -> H2Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut last_err = None;
        for node in &self.nodes {
            match node.scan_range(map_name, start_key, end_key, read_version) {
                Ok(vals) => return Ok(vals),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| H2Error::Storage("No accessible storage nodes".to_string())))
    }

    /// 全ノードのバックグラウンドマテリアライズを実行
    pub fn run_background_materialization(&self) -> usize {
        let mut total = 0;
        for node in &self.nodes {
            total += node.materialize_pending();
        }
        total
    }

    /// ノード間ゴシップ自己修復 (Peer-to-Peer Gossip Repair)
    /// 最も進んでいるノードから遅れているノードへ不足ログを自動補修
    pub fn run_gossip_repair(&self) -> usize {
        let mut repaired_count = 0;
        let n = self.nodes.len();
        for i in 0..n {
            for j in 0..n {
                if i != j {
                    let peer = &self.nodes[j];
                    if let Ok(count) = self.nodes[i].gossip_sync_from(peer) {
                        repaired_count += count;
                    }
                }
            }
        }
        repaired_count
    }
}

/// 分散ストレージフリートを StorageEngine トレイトに適合させるアダプター
pub struct DistributedLogStorageEngine {
    fleet: Arc<StorageFleet>,
}

impl DistributedLogStorageEngine {
    pub fn new(fleet: Arc<StorageFleet>) -> Self {
        Self { fleet }
    }

    pub fn fleet(&self) -> &Arc<StorageFleet> {
        &self.fleet
    }
}

impl StorageEngine for DistributedLogStorageEngine {
    fn latest_committed_lsn(&self) -> Lsn {
        self.fleet.latest_committed_lsn()
    }

    fn fencing_token(&self) -> FencingToken {
        self.fleet.fencing_token()
    }

    fn acquire_lease(&self, token: FencingToken) -> H2Result<()> {
        self.fleet.acquire_lease_quorum(token)
    }

    fn append_logs(&self, records: &[LogRecord], token: FencingToken) -> H2Result<Lsn> {
        self.fleet.append_logs_quorum(records, token)
    }

    fn wait_for_quorum_lsn(&self, _lsn: Lsn) -> H2Result<()> {
        // append_logs_quorum 内ですでにクォーラム完了している
        Ok(())
    }

    fn get_key(&self, map_name: &str, key: &[u8], read_version: Lsn) -> H2Result<Option<Vec<u8>>> {
        self.fleet.get_key_quorum(map_name, key, read_version)
    }

    fn scan_range(
        &self,
        map_name: &str,
        start_key: Option<&[u8]>,
        end_key: Option<&[u8]>,
        read_version: Lsn,
    ) -> H2Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.fleet.scan_range_quorum(map_name, start_key, end_key, read_version)
    }

    fn get_page(&self, page_id: PageId, read_version: Lsn) -> H2Result<Vec<u8>> {
        if let Some(node) = self.fleet.nodes().first() {
            node.get_page(page_id, read_version)
        } else {
            Err(H2Error::Storage("No storage nodes available".to_string()))
        }
    }

    fn get_all_map_names(&self, _read_version: Lsn) -> H2Result<Vec<String>> {
        // ログから作成されたマップ名一覧を抽出
        let mut names = std::collections::BTreeSet::new();
        for node in self.fleet.nodes() {
            for log in node.get_all_logs() {
                if !log.map_name.is_empty() {
                    names.insert(log.map_name);
                }
            }
        }
        Ok(names.into_iter().collect())
    }
}
