use std::sync::Arc;
use h2_mvstore::StorageEngine;
use h2_types::{FencingToken, H2Error, H2Result};

use super::compute::DecoupledComputeNode;
use super::quorum::{DistributedLogStorageEngine, QuorumConfig, StorageFleet};
use super::smart_node::SmartStorageNode;

/// コンピュート・ストレージ完全分離アーキテクチャ (Aurora モデル) クラスター
/// 
/// Primary コンピュートノード、共有ストレージフリート (Quorum)、およびゼロストレージ・リードレプリカ群を
/// 統合的に管理するハイレベルオーケストレーター
pub struct DecoupledCluster {
    pub name: String,
    fleet: Arc<StorageFleet>,
    storage_engine: Arc<DistributedLogStorageEngine>,
    primary: Arc<DecoupledComputeNode>,
    replicas: Vec<Arc<DecoupledComputeNode>>,
}

impl DecoupledCluster {
    /// 3ノード Quorum (2 of 3) クラスターの生成
    pub fn new_3nodes(name: impl Into<String>, num_replicas: usize) -> H2Result<Self> {
        let name_str = name.into();
        let nodes = vec![
            Arc::new(SmartStorageNode::new(1, "az-1")),
            Arc::new(SmartStorageNode::new(2, "az-2")),
            Arc::new(SmartStorageNode::new(3, "az-3")),
        ];

        let config = QuorumConfig::three_nodes();
        let fleet = Arc::new(StorageFleet::new(nodes, config)?);
        let storage_engine = Arc::new(DistributedLogStorageEngine::new(Arc::clone(&fleet)));

        let primary = DecoupledComputeNode::new_primary(
            format!("{}-primary", name_str),
            Arc::clone(&storage_engine) as Arc<dyn StorageEngine>,
            FencingToken(1),
        );

        let mut replicas = Vec::new();
        for i in 1..=num_replicas {
            let replica = DecoupledComputeNode::new_read_replica(
                format!("{}-replica-{}", name_str, i),
                Arc::clone(&storage_engine) as Arc<dyn StorageEngine>,
            );
            primary.register_replica(Arc::clone(&replica));
            replicas.push(replica);
        }

        Ok(Self {
            name: name_str,
            fleet,
            storage_engine,
            primary,
            replicas,
        })
    }

    /// 6ノード Quorum (4 of 6, 3 AZ) エンタープライズ構成クラスターの生成 (Aurora Model)
    pub fn new_6nodes(name: impl Into<String>, num_replicas: usize) -> H2Result<Self> {
        let name_str = name.into();
        let nodes = vec![
            Arc::new(SmartStorageNode::new(1, "az-1")),
            Arc::new(SmartStorageNode::new(2, "az-1")),
            Arc::new(SmartStorageNode::new(3, "az-2")),
            Arc::new(SmartStorageNode::new(4, "az-2")),
            Arc::new(SmartStorageNode::new(5, "az-3")),
            Arc::new(SmartStorageNode::new(6, "az-3")),
        ];

        let config = QuorumConfig::six_nodes();
        let fleet = Arc::new(StorageFleet::new(nodes, config)?);
        let storage_engine = Arc::new(DistributedLogStorageEngine::new(Arc::clone(&fleet)));

        let primary = DecoupledComputeNode::new_primary(
            format!("{}-primary", name_str),
            Arc::clone(&storage_engine) as Arc<dyn StorageEngine>,
            FencingToken(1),
        );

        let mut replicas = Vec::new();
        for i in 1..=num_replicas {
            let replica = DecoupledComputeNode::new_read_replica(
                format!("{}-replica-{}", name_str, i),
                Arc::clone(&storage_engine) as Arc<dyn StorageEngine>,
            );
            primary.register_replica(Arc::clone(&replica));
            replicas.push(replica);
        }

        Ok(Self {
            name: name_str,
            fleet,
            storage_engine,
            primary,
            replicas,
        })
    }

    pub fn primary(&self) -> &Arc<DecoupledComputeNode> {
        &self.primary
    }

    /// Primary コンピュートノードの SQL 接続を取得
    pub fn primary_connection(&self) -> crate::Connection {
        self.primary.connection()
    }

    pub fn replicas(&self) -> &[Arc<DecoupledComputeNode>] {
        &self.replicas
    }

    pub fn replica(&self, index: usize) -> Option<&Arc<DecoupledComputeNode>> {
        self.replicas.get(index)
    }

    /// 指定インデックスの Read Replica の SQL 接続を取得 (ReadOnly)
    pub fn replica_connection(&self, index: usize) -> H2Result<crate::Connection> {
        let r = self.replica(index).ok_or_else(|| {
            H2Error::Storage(format!("Replica index {} not found", index))
        })?;
        Ok(r.connection())
    }

    pub fn fleet(&self) -> &Arc<StorageFleet> {
        &self.fleet
    }

    pub fn storage_engine(&self) -> &Arc<DistributedLogStorageEngine> {
        &self.storage_engine
    }

    /// 動的リードレプリカ追加（追加ストレージコスト 0）
    pub fn add_read_replica(&mut self, replica_id: impl Into<String>) -> Arc<DecoupledComputeNode> {
        let replica = DecoupledComputeNode::new_read_replica(
            replica_id,
            Arc::clone(&self.storage_engine) as Arc<dyn StorageEngine>,
        );
        self.primary.register_replica(Arc::clone(&replica));
        self.replicas.push(Arc::clone(&replica));
        replica
    }

    /// 全ストレージノードの非同期 Redo マテリアライズを実行
    pub fn step_materialization(&self) -> usize {
        self.fleet.run_background_materialization()
    }

    /// ストレージノード間のゴシップ自己修復を実行
    pub fn step_gossip_repair(&self) -> usize {
        self.fleet.run_gossip_repair()
    }

    /// フェイルオーバー: 指定したリードレプリカを Primary に瞬間昇格
    pub fn failover_to_replica(&mut self, replica_index: usize) -> H2Result<FencingToken> {
        if replica_index >= self.replicas.len() {
            return Err(H2Error::Storage("Replica index out of bounds".to_string()));
        }

        let new_primary = self.replicas.remove(replica_index);
        let token = new_primary.promote_to_primary()?;

        // 他のすべてのレプリカを新 Primary にリスナー登録
        for r in &self.replicas {
            new_primary.register_replica(Arc::clone(r));
        }

        self.primary = new_primary;
        Ok(token)
    }
}
