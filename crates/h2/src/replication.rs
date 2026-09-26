use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info, warn};

use h2_mvstore::replication::{
    DefaultChangeCollector, ReplicationChange, ReplicationCommitRecord,
    ReplicationListener,
};
use h2_mvstore::MVStore;
use h2_sql::SQLEngine;
use h2_types::{H2Error, H2Result};

use crate::Connection;

/// 同期レプリケーションの整合性モード
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncReplicationMode {
    /// PostgreSQL の synchronous_commit = remote_apply 相当:
    /// スタンバイが変更を受信し、ローカルストレージおよびカタログに適用（apply）完了するまで
    /// プライマリのコミット呼び出し元をブロック待機する。
    RemoteApply,
}

/// インスタンスの動作ロール
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceRole {
    /// 単一インスタンス（レプリケーションなし）
    Standalone,
    /// プライマリ（Read-Write インスタンス）
    Primary {
        listen_addr: SocketAddr,
        sync_mode: SyncReplicationMode,
        apply_timeout: Duration,
    },
    /// スタンバイ（Read-Only インスタンス）
    Standby {
        primary_addr: SocketAddr,
    },
}

/// インスタンス起動設定
#[derive(Debug, Clone)]
pub struct InstanceConfig {
    pub role: InstanceRole,
}

impl Default for InstanceConfig {
    fn default() -> Self {
        Self {
            role: InstanceRole::Standalone,
        }
    }
}

impl InstanceConfig {
    pub fn primary(listen_addr: SocketAddr) -> Self {
        Self {
            role: InstanceRole::Primary {
                listen_addr,
                sync_mode: SyncReplicationMode::RemoteApply,
                apply_timeout: Duration::from_secs(10),
            },
        }
    }

    pub fn primary_with_timeout(listen_addr: SocketAddr, apply_timeout: Duration) -> Self {
        Self {
            role: InstanceRole::Primary {
                listen_addr,
                sync_mode: SyncReplicationMode::RemoteApply,
                apply_timeout,
            },
        }
    }

    pub fn standby(primary_addr: SocketAddr) -> Self {
        Self {
            role: InstanceRole::Standby { primary_addr },
        }
    }
}

/// レプリケーション通信メッセージ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReplicationMessage {
    /// 初期スナップショット要求
    SnapshotRequest,
    /// 初期スナップショット応答
    SnapshotResponse {
        current_version: u64,
        maps: HashMap<String, Vec<(Vec<u8>, Vec<u8>)>>,
    },
    /// コミットレコード送信（Primary -> Standby）
    Commit(ReplicationCommitRecord),
    /// 適用完了通知 (Standby -> Primary: remote_apply)
    Applied { commit_version: u64 },
    /// ハートビート
    Ping,
    Pong,
}

async fn write_msg<W: AsyncWriteExt + Unpin>(writer: &mut W, msg: &ReplicationMessage) -> H2Result<()> {
    let bytes = serde_json::to_vec(msg).map_err(|e| H2Error::Serialization(e.to_string()))?;
    let len = bytes.len() as u32;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_msg<R: AsyncReadExt + Unpin>(reader: &mut R) -> H2Result<ReplicationMessage> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    let msg: ReplicationMessage =
        serde_json::from_slice(&buf).map_err(|e| H2Error::Serialization(e.to_string()))?;
    Ok(msg)
}

/// プライマリ側で各スタンバイへのブロードキャストと ACK を管理するハブ
pub struct PrimaryReplicationHub {
    sync_mode: SyncReplicationMode,
    apply_timeout: Duration,
    /// 接続中の全スタンバイへコミットレコードを送るブロードキャスト送信チャネル
    commit_tx: tokio::sync::broadcast::Sender<ReplicationCommitRecord>,
    /// スタンバイから適用完了報告があった最新バージョン
    applied_version: Arc<AtomicU64>,
    /// applied_version の更新を待つための Condvar
    apply_condvar: Arc<(Mutex<u64>, Condvar)>,
    /// スタンバイが接続されているかどうか
    standby_connected: Arc<AtomicBool>,
}

impl PrimaryReplicationHub {
    pub fn new(sync_mode: SyncReplicationMode, apply_timeout: Duration) -> Arc<Self> {
        let (commit_tx, _) = tokio::sync::broadcast::channel(1024);
        Arc::new(Self {
            sync_mode,
            apply_timeout,
            commit_tx,
            applied_version: Arc::new(AtomicU64::new(0)),
            apply_condvar: Arc::new((Mutex::new(0), Condvar::new())),
            standby_connected: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn notify_applied(&self, version: u64) {
        self.applied_version.fetch_max(version, Ordering::SeqCst);
        let (lock, cvar) = &*self.apply_condvar;
        let mut cur = lock.lock();
        if version > *cur {
            *cur = version;
            cvar.notify_all();
        }
    }
}

impl ReplicationListener for PrimaryReplicationHub {
    fn on_commit(&self, commit_version: u64, changes: Vec<ReplicationChange>) -> H2Result<()> {
        if changes.is_empty() {
            return Ok(());
        }

        // スタンバイが接続されていない場合（初期化中など）はブロックせずにコミット
        if !self.standby_connected.load(Ordering::SeqCst) {
            return Ok(());
        }

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let record = ReplicationCommitRecord {
            commit_version,
            changes,
            timestamp_ms,
        };

        // スタンバイへ送信
        let _ = self.commit_tx.send(record);

        // remote_apply: スタンバイでの適用完了（ACK）を待機
        if self.sync_mode == SyncReplicationMode::RemoteApply {
            let (lock, cvar) = &*self.apply_condvar;
            let mut cur = lock.lock();
            let start = std::time::Instant::now();

            while *cur < commit_version {
                let elapsed = start.elapsed();
                if elapsed >= self.apply_timeout {
                    return Err(H2Error::Replication(format!(
                        "remote_apply replication timeout waiting for version {} (current applied: {})",
                        commit_version, *cur
                    )));
                }
                let remaining = self.apply_timeout - elapsed;
                let res = cvar.wait_for(&mut cur, remaining);
                if res.timed_out() && *cur < commit_version {
                    return Err(H2Error::Replication(format!(
                        "remote_apply replication timeout waiting for version {} (current applied: {})",
                        commit_version, *cur
                    )));
                }
            }
        }

        Ok(())
    }
}

/// データベースインスタンス（Primary または Standby、または Standalone）
pub struct Instance {
    store: Arc<MVStore>,
    engine: Arc<SQLEngine>,
    config: InstanceConfig,
    actual_listen_addr: Option<SocketAddr>,
    shutdown_trigger: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Instance {
    /// ファイルベースでインスタンスを開く
    pub fn open<P: AsRef<Path>>(path: P, config: InstanceConfig) -> H2Result<Self> {
        let store = Arc::new(MVStore::open(path)?);
        Self::init(store, config)
    }

    /// インメモリでインスタンスを開く
    pub fn open_in_memory(config: InstanceConfig) -> H2Result<Self> {
        let store = Arc::new(MVStore::open_in_memory());
        Self::init(store, config)
    }

    fn init(store: Arc<MVStore>, config: InstanceConfig) -> H2Result<Self> {
        let engine = Arc::new(SQLEngine::new(Arc::clone(&store))?);
        let mut actual_listen_addr = None;
        let mut shutdown_trigger = None;

        match &config.role {
            InstanceRole::Standalone => {
                // 通常のスタンドアロン
            }
            InstanceRole::Primary {
                listen_addr,
                sync_mode,
                apply_timeout,
            } => {
                // Read-Write モード
                engine.set_read_only(false);

                // 変更コレクターとリスナーを設定
                let collector = Arc::new(DefaultChangeCollector::new());
                store.set_change_sink(Some(collector));

                let hub = PrimaryReplicationHub::new(*sync_mode, *apply_timeout);
                store.set_replication_listener(Some(Arc::clone(&hub) as Arc<dyn ReplicationListener>));

                // TCP サーバーを起動
                let rt = tokio::runtime::Handle::try_current();
                let (shut_tx, shut_rx) = tokio::sync::oneshot::channel();
                shutdown_trigger = Some(shut_tx);

                let listen_target = *listen_addr;
                let store_clone = Arc::clone(&store);
                let hub_clone = Arc::clone(&hub);

                let (bound_tx, bound_rx) = std::sync::mpsc::channel();

                let runner = async move {
                    let listener = match TcpListener::bind(listen_target).await {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = bound_tx.send(Err(H2Error::Io(e)));
                            return;
                        }
                    };
                    let local_addr = listener.local_addr().unwrap();
                    let _ = bound_tx.send(Ok(local_addr));

                    info!("Primary replication server listening on {}", local_addr);
                    tokio::select! {
                        _ = shut_rx => {
                            debug!("Primary replication listener shutting down");
                        }
                        _ = run_primary_server(listener, store_clone, hub_clone) => {}
                    }
                };

                if let Ok(handle) = rt {
                    handle.spawn(runner);
                } else {
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_multi_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        rt.block_on(runner);
                    });
                }

                match bound_rx.recv_timeout(Duration::from_secs(5)) {
                    Ok(Ok(addr)) => {
                        actual_listen_addr = Some(addr);
                    }
                    Ok(Err(e)) => return Err(e),
                    Err(_) => {
                        return Err(H2Error::Replication("Timeout binding primary replication socket".to_string()))
                    }
                }
            }
            InstanceRole::Standby { primary_addr } => {
                // Read-Only モードに設定
                engine.set_read_only(true);

                let rt = tokio::runtime::Handle::try_current();
                let (shut_tx, shut_rx) = tokio::sync::oneshot::channel();
                shutdown_trigger = Some(shut_tx);

                let primary_target = *primary_addr;
                let store_clone = Arc::clone(&store);
                let engine_clone = Arc::clone(&engine);

                let (ready_tx, ready_rx) = std::sync::mpsc::channel();

                let runner = async move {
                    tokio::select! {
                        _ = shut_rx => {
                            debug!("Standby replication client shutting down");
                        }
                        _ = run_standby_client(primary_target, store_clone, engine_clone, ready_tx) => {}
                    }
                };

                if let Ok(handle) = rt {
                    handle.spawn(runner);
                } else {
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_multi_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        rt.block_on(runner);
                    });
                }

                // スタンバイの初期同期完了・接続待機
                match ready_rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return Err(e),
                    Err(_) => {
                        return Err(H2Error::Replication(
                            "Timeout connecting standby to primary replication server".to_string(),
                        ))
                    }
                }
            }
        }

        Ok(Self {
            store,
            engine,
            config,
            actual_listen_addr,
            shutdown_trigger,
        })
    }

    /// 新しいクライアント接続ハンドルを生成
    pub fn connect(&self) -> H2Result<Connection> {
        Ok(Connection::from_engine(Arc::clone(&self.engine), Arc::clone(&self.store)))
    }

    pub fn is_read_only(&self) -> bool {
        self.engine.is_read_only()
    }

    pub fn role(&self) -> &InstanceRole {
        &self.config.role
    }

    pub fn replication_addr(&self) -> Option<SocketAddr> {
        self.actual_listen_addr
    }

    /// 高速バイナリ形式でインスタンスから整合バックアップを取得（Primary / Standby 双方に対応）
    pub fn backup<P: AsRef<Path>>(&self, path: P) -> H2Result<h2_mvstore::BackupMetadata> {
        let is_replica = self.is_read_only();
        self.store.dump_backup_with_role(path, is_replica)
    }

    /// バックアップファイルの整合性を検証（データ書き換えなし）
    pub fn verify_backup<P: AsRef<Path>>(&self, path: P) -> H2Result<h2_mvstore::BackupMetadata> {
        self.store.verify_backup(path)
    }

    pub fn close(mut self) {
        if let Some(shut) = self.shutdown_trigger.take() {
            let _ = shut.send(());
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(shut) = self.shutdown_trigger.take() {
            let _ = shut.send(());
        }
    }
}

/// Primary 側のレプリケーション待機ループ
async fn run_primary_server(
    listener: TcpListener,
    store: Arc<MVStore>,
    hub: Arc<PrimaryReplicationHub>,
) {
    loop {
        let (socket, client_addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                error!("Error accepting standby connection: {:?}", e);
                break;
            }
        };

        info!("Standby connected from {}", client_addr);
        hub.standby_connected.store(true, Ordering::SeqCst);

        let store_clone = Arc::clone(&store);
        let hub_clone = Arc::clone(&hub);

        tokio::spawn(async move {
            if let Err(e) = handle_standby_connection(socket, store_clone, hub_clone).await {
                warn!("Standby connection closed with: {:?}", e);
            }
        });
    }
}

/// Primary 側でのスタンバイ 1 接続の処理
async fn handle_standby_connection(
    mut stream: TcpStream,
    store: Arc<MVStore>,
    hub: Arc<PrimaryReplicationHub>,
) -> H2Result<()> {
    let mut commit_rx = hub.commit_tx.subscribe();

    loop {
        tokio::select! {
            // プライマリからコミット通知が来たらスタンバイへ送信
            commit_res = commit_rx.recv() => {
                match commit_res {
                    Ok(record) => {
                        write_msg(&mut stream, &ReplicationMessage::Commit(record)).await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Standby lagged by {} records", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            // スタンバイからメッセージ（SnapshotRequest, Applied, Ping など）を受信
            msg_res = read_msg(&mut stream) => {
                let msg = msg_res?;
                match msg {
                    ReplicationMessage::SnapshotRequest => {
                        let maps = store.scan_all_maps();
                        let current_version = store.current_version();
                        write_msg(
                            &mut stream,
                            &ReplicationMessage::SnapshotResponse {
                                current_version,
                                maps,
                            },
                        ).await?;
                    }
                    ReplicationMessage::Applied { commit_version } => {
                        // remote_apply ACK をハブに通知
                        hub.notify_applied(commit_version);
                    }
                    ReplicationMessage::Ping => {
                        write_msg(&mut stream, &ReplicationMessage::Pong).await?;
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

/// Standby 側のレプリケーション接続＆同期ループ
async fn run_standby_client(
    primary_addr: SocketAddr,
    store: Arc<MVStore>,
    engine: Arc<SQLEngine>,
    ready_tx: std::sync::mpsc::Sender<H2Result<()>>,
) {
    let mut stream = match TcpStream::connect(primary_addr).await {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(H2Error::Io(e)));
            return;
        }
    };

    // 1. 初期スナップショットを要求
    if let Err(e) = write_msg(&mut stream, &ReplicationMessage::SnapshotRequest).await {
        let _ = ready_tx.send(Err(e));
        return;
    }

    // 2. スナップショットを受信して適用
    match read_msg(&mut stream).await {
        Ok(ReplicationMessage::SnapshotResponse { current_version, maps }) => {
            if let Err(e) = store.apply_snapshot(current_version, maps) {
                let _ = ready_tx.send(Err(e));
                return;
            }
            if let Err(e) = engine.catalog().reload() {
                let _ = ready_tx.send(Err(e));
                return;
            }
            // 初期同期完了を通知
            let _ = ready_tx.send(Ok(()));
        }
        Ok(other) => {
            let _ = ready_tx.send(Err(H2Error::Replication(format!(
                "Unexpected initial message from primary: {:?}",
                other
            ))));
            return;
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    }

    // 3. コミット受信ループ
    loop {
        let msg = match read_msg(&mut stream).await {
            Ok(m) => m,
            Err(e) => {
                warn!("Standby disconnected from primary: {:?}", e);
                break;
            }
        };

        match msg {
            ReplicationMessage::Commit(record) => {
                let mut catalog_updated = false;

                // 各マップの変更をアプライ
                for change in record.changes {
                    if change.map_name == "_catalog" {
                        catalog_updated = true;
                    }

                    if change.is_clear {
                        store.open_map(&change.map_name).clear();
                    } else if let Some(val) = change.value {
                        store.open_map(&change.map_name).put(change.key, val);
                    } else {
                        store.open_map(&change.map_name).remove(&change.key);
                    }
                }

                // スタンバイのストレージをコミットし、バージョンをプライマリと同期
                store.set_version(record.commit_version.saturating_sub(1));
                if let Err(e) = store.commit() {
                    error!("Error committing changes on standby: {:?}", e);
                    break;
                }
                store.set_version(record.commit_version);

                // カタログ変更が含まれていた場合はインメモリキャッシュを即座にリロード
                if catalog_updated {
                    if let Err(e) = engine.catalog().reload() {
                        error!("Error reloading catalog on standby: {:?}", e);
                    }
                }

                // remote_apply ACK をプライマリへ返送！
                let ack = ReplicationMessage::Applied {
                    commit_version: record.commit_version,
                };
                if let Err(e) = write_msg(&mut stream, &ack).await {
                    error!("Error sending Applied ACK from standby: {:?}", e);
                    break;
                }
            }
            ReplicationMessage::Ping => {
                let _ = write_msg(&mut stream, &ReplicationMessage::Pong).await;
            }
            _ => {}
        }
    }
}
