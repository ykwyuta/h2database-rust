use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use h2_mvstore::{MVStore, Transaction};
use h2_types::{H2Error, H2Result, Value};

use crate::model::{Edge, Node};

pub struct GraphStore {
    store: Arc<MVStore>,
    graph_name: String,
    next_node_id: AtomicU64,
    next_edge_id: AtomicU64,
}

impl GraphStore {
    pub fn new(store: Arc<MVStore>, graph_name: impl Into<String>) -> H2Result<Self> {
        let graph_name = graph_name.into();
        let meta_map = format!("_g_{}_meta", graph_name);
        let node_id_map = store.open_map(&meta_map);

        let init_node_id = node_id_map
            .get(b"next_node_id")
            .and_then(|b| if b.len() == 8 { Some(u64::from_be_bytes(b.as_slice().try_into().unwrap())) } else { None })
            .unwrap_or(1);

        let init_edge_id = node_id_map
            .get(b"next_edge_id")
            .and_then(|b| if b.len() == 8 { Some(u64::from_be_bytes(b.as_slice().try_into().unwrap())) } else { None })
            .unwrap_or(1);

        Ok(Self {
            store,
            graph_name,
            next_node_id: AtomicU64::new(init_node_id),
            next_edge_id: AtomicU64::new(init_edge_id),
        })
    }

    pub fn graph_name(&self) -> &str {
        &self.graph_name
    }

    pub fn store(&self) -> &Arc<MVStore> {
        &self.store
    }

    fn nodes_map(&self) -> String {
        format!("_g_{}_nodes", self.graph_name)
    }

    fn edges_map(&self) -> String {
        format!("_g_{}_edges", self.graph_name)
    }

    fn adj_out_map(&self) -> String {
        format!("_g_{}_adj_out", self.graph_name)
    }

    fn adj_in_map(&self) -> String {
        format!("_g_{}_adj_in", self.graph_name)
    }

    fn idx_label_map(&self) -> String {
        format!("_g_{}_idx_label", self.graph_name)
    }

    fn meta_map(&self) -> String {
        format!("_g_{}_meta", self.graph_name)
    }

    fn alloc_node_id(&self, tx: &Transaction) -> H2Result<u64> {
        let id = self.next_node_id.fetch_add(1, Ordering::SeqCst);
        let next = id + 1;
        tx.put(&self.meta_map(), b"next_node_id".to_vec(), next.to_be_bytes().to_vec())?;
        Ok(id)
    }

    fn alloc_edge_id(&self, tx: &Transaction) -> H2Result<u64> {
        let id = self.next_edge_id.fetch_add(1, Ordering::SeqCst);
        let next = id + 1;
        tx.put(&self.meta_map(), b"next_edge_id".to_vec(), next.to_be_bytes().to_vec())?;
        Ok(id)
    }

    // ================= ノード操作 =================

    pub fn create_node(
        &self,
        tx: &Transaction,
        labels: Vec<String>,
        properties: HashMap<String, Value>,
    ) -> H2Result<Node> {
        let id = self.alloc_node_id(tx)?;
        let node = Node::new(id, labels, properties);
        self.save_node(tx, &node)?;
        Ok(node)
    }

    pub fn save_node(&self, tx: &Transaction, node: &Node) -> H2Result<()> {
        let bytes = bincode::serialize(node)
            .map_err(|e| H2Error::Serialization(format!("Node serialization error: {e}")))?;
        tx.put(&self.nodes_map(), node.id.to_be_bytes().to_vec(), bytes)?;

        // ラベル・プロパティインデックスを更新
        let idx_map = self.idx_label_map();
        for label in &node.labels {
            // (label, "", Null, node_id)
            let key = encode_label_idx_key(label, None, None, Some(node.id));
            tx.put(&idx_map, key, node.id.to_be_bytes().to_vec())?;

            for (prop_name, prop_val) in &node.properties {
                let key = encode_label_idx_key(label, Some(prop_name), Some(prop_val), Some(node.id));
                tx.put(&idx_map, key, node.id.to_be_bytes().to_vec())?;
            }
        }

        Ok(())
    }

    pub fn get_node(&self, tx: &Transaction, id: u64) -> H2Result<Option<Node>> {
        let key = id.to_be_bytes();
        let bytes = match tx.get(&self.nodes_map(), &key)? {
            Some(b) => b,
            None => return Ok(None),
        };
        let node: Node = bincode::deserialize(&bytes)
            .map_err(|e| H2Error::Serialization(format!("Node deserialization error: {e}")))?;
        Ok(Some(node))
    }

    pub fn delete_node(&self, tx: &Transaction, id: u64, detach: bool) -> H2Result<bool> {
        let node = match self.get_node(tx, id)? {
            Some(n) => n,
            None => return Ok(false),
        };

        let out_edges = self.get_out_edges(tx, id, None)?;
        let in_edges = self.get_in_edges(tx, id, None)?;

        if !detach && (!out_edges.is_empty() || !in_edges.is_empty()) {
            return Err(H2Error::Execution(format!(
                "Cannot delete node {id} because it still has relationships. Use DETACH DELETE to remove relationships as well."
            )));
        }

        for edge in out_edges.into_iter().chain(in_edges.into_iter()) {
            self.delete_edge(tx, edge.id)?;
        }

        // ラベルインデックス削除
        let idx_map = self.idx_label_map();
        for label in &node.labels {
            let key = encode_label_idx_key(label, None, None, Some(node.id));
            tx.remove(&idx_map, &key)?;
            for (prop_name, prop_val) in &node.properties {
                let key = encode_label_idx_key(label, Some(prop_name), Some(prop_val), Some(node.id));
                tx.remove(&idx_map, &key)?;
            }
        }

        tx.remove(&self.nodes_map(), &id.to_be_bytes())
    }

    pub fn all_nodes(&self, tx: &Transaction) -> H2Result<Vec<Node>> {
        let entries = tx.scan_visible(&self.nodes_map())?;
        let mut nodes = Vec::with_capacity(entries.len());
        for (_k, v) in entries {
            let node: Node = bincode::deserialize(&v)
                .map_err(|e| H2Error::Serialization(format!("Node deserialization error: {e}")))?;
            nodes.push(node);
        }
        Ok(nodes)
    }

    pub fn find_nodes_by_label(&self, tx: &Transaction, label: &str) -> H2Result<Vec<Node>> {
        let prefix = encode_label_idx_prefix(label, None, None);
        let entries = tx.scan_prefix_visible(&self.idx_label_map(), &prefix)?;
        let mut nodes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (_k, v) in entries {
            if v.len() == 8 {
                let node_id = u64::from_be_bytes(v.as_slice().try_into().unwrap());
                if seen.insert(node_id) {
                    if let Some(node) = self.get_node(tx, node_id)? {
                        nodes.push(node);
                    }
                }
            }
        }
        Ok(nodes)
    }

    pub fn find_nodes_by_label_and_prop(
        &self,
        tx: &Transaction,
        label: &str,
        prop_key: &str,
        prop_val: &Value,
    ) -> H2Result<Vec<Node>> {
        let prefix = encode_label_idx_prefix(label, Some(prop_key), Some(prop_val));
        let entries = tx.scan_prefix_visible(&self.idx_label_map(), &prefix)?;
        let mut nodes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (_k, v) in entries {
            if v.len() == 8 {
                let node_id = u64::from_be_bytes(v.as_slice().try_into().unwrap());
                if seen.insert(node_id) {
                    if let Some(node) = self.get_node(tx, node_id)? {
                        nodes.push(node);
                    }
                }
            }
        }
        Ok(nodes)
    }

    // ================= エッジ操作 =================

    pub fn create_edge(
        &self,
        tx: &Transaction,
        edge_type: impl Into<String>,
        src_id: u64,
        dst_id: u64,
        properties: HashMap<String, Value>,
    ) -> H2Result<Edge> {
        if self.get_node(tx, src_id)?.is_none() {
            return Err(H2Error::Execution(format!("Source node {src_id} not found")));
        }
        if self.get_node(tx, dst_id)?.is_none() {
            return Err(H2Error::Execution(format!("Target node {dst_id} not found")));
        }

        let id = self.alloc_edge_id(tx)?;
        let edge = Edge::new(id, edge_type, src_id, dst_id, properties);
        self.save_edge(tx, &edge)?;
        Ok(edge)
    }

    pub fn save_edge(&self, tx: &Transaction, edge: &Edge) -> H2Result<()> {
        let bytes = bincode::serialize(edge)
            .map_err(|e| H2Error::Serialization(format!("Edge serialization error: {e}")))?;
        tx.put(&self.edges_map(), edge.id.to_be_bytes().to_vec(), bytes)?;

        // 隣接リスト更新
        let out_key = encode_adj_key(edge.src_id, Some(&edge.edge_type), Some(edge.id));
        tx.put(&self.adj_out_map(), out_key, edge.dst_id.to_be_bytes().to_vec())?;

        let in_key = encode_adj_key(edge.dst_id, Some(&edge.edge_type), Some(edge.id));
        tx.put(&self.adj_in_map(), in_key, edge.src_id.to_be_bytes().to_vec())?;

        Ok(())
    }

    pub fn get_edge(&self, tx: &Transaction, id: u64) -> H2Result<Option<Edge>> {
        let key = id.to_be_bytes();
        let bytes = match tx.get(&self.edges_map(), &key)? {
            Some(b) => b,
            None => return Ok(None),
        };
        let edge: Edge = bincode::deserialize(&bytes)
            .map_err(|e| H2Error::Serialization(format!("Edge deserialization error: {e}")))?;
        Ok(Some(edge))
    }

    pub fn delete_edge(&self, tx: &Transaction, id: u64) -> H2Result<bool> {
        let edge = match self.get_edge(tx, id)? {
            Some(e) => e,
            None => return Ok(false),
        };

        let out_key = encode_adj_key(edge.src_id, Some(&edge.edge_type), Some(edge.id));
        tx.remove(&self.adj_out_map(), &out_key)?;

        let in_key = encode_adj_key(edge.dst_id, Some(&edge.edge_type), Some(edge.id));
        tx.remove(&self.adj_in_map(), &in_key)?;

        tx.remove(&self.edges_map(), &id.to_be_bytes())
    }

    pub fn all_edges(&self, tx: &Transaction) -> H2Result<Vec<Edge>> {
        let entries = tx.scan_visible(&self.edges_map())?;
        let mut edges = Vec::with_capacity(entries.len());
        for (_k, v) in entries {
            let edge: Edge = bincode::deserialize(&v)
                .map_err(|e| H2Error::Serialization(format!("Edge deserialization error: {e}")))?;
            edges.push(edge);
        }
        Ok(edges)
    }

    pub fn get_out_edges(
        &self,
        tx: &Transaction,
        src_id: u64,
        edge_type: Option<&str>,
    ) -> H2Result<Vec<Edge>> {
        let prefix = encode_adj_key(src_id, edge_type, None);
        let entries = tx.scan_prefix_visible(&self.adj_out_map(), &prefix)?;
        let mut edges = Vec::with_capacity(entries.len());
        for (k, _) in entries {
            if let Some((_src, _typ, edge_id)) = decode_adj_key(&k) {
                if let Some(edge) = self.get_edge(tx, edge_id)? {
                    edges.push(edge);
                }
            }
        }
        Ok(edges)
    }

    pub fn get_in_edges(
        &self,
        tx: &Transaction,
        dst_id: u64,
        edge_type: Option<&str>,
    ) -> H2Result<Vec<Edge>> {
        let prefix = encode_adj_key(dst_id, edge_type, None);
        let entries = tx.scan_prefix_visible(&self.adj_in_map(), &prefix)?;
        let mut edges = Vec::with_capacity(entries.len());
        for (k, _) in entries {
            if let Some((_dst, _typ, edge_id)) = decode_adj_key(&k) {
                if let Some(edge) = self.get_edge(tx, edge_id)? {
                    edges.push(edge);
                }
            }
        }
        Ok(edges)
    }
}

// ================= キーエンコード関数 =================

fn encode_adj_key(node_id: u64, edge_type: Option<&str>, edge_id: Option<u64>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(32);
    buf.extend_from_slice(&node_id.to_be_bytes());
    if let Some(t) = edge_type {
        buf.push(0x01); // separator
        buf.extend_from_slice(t.as_bytes());
        buf.push(0x00); // null terminator
        if let Some(e_id) = edge_id {
            buf.extend_from_slice(&e_id.to_be_bytes());
        }
    } else if let Some(e_id) = edge_id {
        buf.push(0x02);
        buf.extend_from_slice(&e_id.to_be_bytes());
    }
    buf
}

fn decode_adj_key(bytes: &[u8]) -> Option<(u64, String, u64)> {
    if bytes.len() < 8 {
        return None;
    }
    let node_id = u64::from_be_bytes(bytes[0..8].try_into().unwrap());
    if bytes.len() == 8 {
        return None;
    }
    if bytes[8] == 0x01 {
        let mut pos = 9;
        while pos < bytes.len() && bytes[pos] != 0x00 {
            pos += 1;
        }
        if pos < bytes.len() && bytes.len() >= pos + 1 + 8 {
            let edge_type = String::from_utf8_lossy(&bytes[9..pos]).into_owned();
            let edge_id = u64::from_be_bytes(bytes[pos + 1..pos + 9].try_into().unwrap());
            return Some((node_id, edge_type, edge_id));
        }
    } else if bytes[8] == 0x02 && bytes.len() >= 17 {
        let edge_id = u64::from_be_bytes(bytes[9..17].try_into().unwrap());
        return Some((node_id, String::new(), edge_id));
    }
    None
}

fn encode_label_idx_prefix(label: &str, prop_key: Option<&str>, prop_val: Option<&Value>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(label.as_bytes());
    buf.push(0x00);
    if let Some(key) = prop_key {
        buf.extend_from_slice(key.as_bytes());
        buf.push(0x00);
        if let Some(val) = prop_val {
            let val_bytes = serde_json::to_vec(val).unwrap_or_default();
            buf.extend_from_slice(&val_bytes);
            buf.push(0x00);
        }
    }
    buf
}

fn encode_label_idx_key(
    label: &str,
    prop_key: Option<&str>,
    prop_val: Option<&Value>,
    node_id: Option<u64>,
) -> Vec<u8> {
    let mut buf = encode_label_idx_prefix(label, prop_key, prop_val);
    if let Some(id) = node_id {
        buf.extend_from_slice(&id.to_be_bytes());
    }
    buf
}
