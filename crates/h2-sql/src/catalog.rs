use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use parking_lot::RwLock;

use h2_mvstore::{MVMap, MVStore};
use h2_types::{DataType, H2Error, H2Result};
use crate::procedural::RoutineDef;

fn default_schema() -> String {
    "public".to_string()
}

/// カラム統計情報
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnStats {
    pub ndv: u64,
    pub null_frac: f64,
    pub avg_width: f64,
    pub most_common_vals: Vec<h2_types::Value>,
    pub most_common_freqs: Vec<f64>,
}

fn default_sample_ratio() -> f64 {
    1.0
}

/// テーブル統計情報
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableStats {
    pub row_count: u64,
    pub dead_row_count: u64,
    pub last_analyzed: Option<u64>,
    pub total_pages: u64,
    #[serde(default = "default_sample_ratio")]
    pub sample_ratio: f64,
}

/// カラム定義
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub is_nullable: bool,
    pub is_primary_key: bool,
    #[serde(default)]
    pub physical_index: Option<usize>,
    #[serde(default)]
    pub sequence_name: Option<String>,
    #[serde(default)]
    pub stats: Option<ColumnStats>,
}

impl ColumnDef {
    pub fn new(name: impl Into<String>, data_type: DataType, is_nullable: bool, is_primary_key: bool) -> Self {
        Self {
            name: name.into(),
            data_type,
            is_nullable,
            is_primary_key,
            physical_index: None,
            sequence_name: None,
            stats: None,
        }
    }
}

/// シーケンス定義
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceDef {
    pub name: String,
    #[serde(default = "default_schema")]
    pub schema: String,
    pub current_value: i64,
    pub increment_by: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub start_with: i64,
    pub cycle: bool,
    pub is_called: bool,
    #[serde(default)]
    pub owner_table: Option<String>,
}

impl SequenceDef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            schema: default_schema(),
            current_value: 1,
            increment_by: 1,
            min_value: 1,
            max_value: i64::MAX,
            start_with: 1,
            cycle: false,
            is_called: false,
            owner_table: None,
        }
    }

    pub fn full_name(&self) -> String {
        format!("{}.{}", self.schema.to_lowercase(), self.name.to_lowercase())
    }
}

/// 外部キーアクション
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForeignKeyAction {
    Restrict,
    Cascade,
    SetNull,
    NoAction,
}

/// 外部キー定義
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKeyDef {
    pub name: Option<String>,
    pub column: String,
    pub foreign_table: String,
    pub foreign_column: String,
    pub on_delete: ForeignKeyAction,
    pub on_update: ForeignKeyAction,
}

/// 一意制約定義
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniqueConstraintDef {
    pub name: Option<String>,
    pub columns: Vec<String>,
}

/// テーブル定義
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableDef {
    pub name: String,
    #[serde(default = "default_schema")]
    pub schema: String,
    pub columns: Vec<ColumnDef>,
    pub next_row_id: u64,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKeyDef>,
    #[serde(default)]
    pub primary_key: Vec<String>,
    #[serde(default)]
    pub unique_constraints: Vec<UniqueConstraintDef>,
    #[serde(default)]
    pub is_queue: bool,
    #[serde(default)]
    pub retention_duration_ms: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<u64>,
    #[serde(default)]
    pub stats: Option<TableStats>,
    #[serde(default)]
    pub approx_row_count: i64,
    #[serde(default)]
    pub is_cache: bool,
    #[serde(default)]
    pub cache_ttl_ms: Option<u64>,
    #[serde(default)]
    pub write_back_table: Option<String>,
    #[serde(default)]
    pub write_back_interval_ms: Option<u64>,
    #[serde(default)]
    pub write_back_mode: Option<String>,
    #[serde(default)]
    pub is_unlogged: bool,
    #[serde(default)]
    pub is_iceberg: bool,
    #[serde(default)]
    pub iceberg_location: Option<String>,
}

impl TableDef {
    pub fn new(name: impl Into<String>, mut columns: Vec<ColumnDef>) -> Self {
        for (idx, col) in columns.iter_mut().enumerate() {
            if col.physical_index.is_none() {
                col.physical_index = Some(idx);
            }
        }
        let pk_cols: Vec<String> = columns
            .iter()
            .filter(|c| c.is_primary_key)
            .map(|c| c.name.clone())
            .collect();
        Self {
            name: name.into(),
            schema: default_schema(),
            columns,
            next_row_id: 1,
            foreign_keys: Vec::new(),
            primary_key: pk_cols,
            unique_constraints: Vec::new(),
            is_queue: false,
            retention_duration_ms: None,
            max_bytes: None,
            stats: None,
            approx_row_count: 0,
            is_cache: false,
            cache_ttl_ms: None,
            write_back_table: None,
            write_back_interval_ms: None,
            write_back_mode: None,
            is_unlogged: false,
            is_iceberg: false,
            iceberg_location: None,
        }
    }

    pub fn new_queue(
        name: impl Into<String>,
        user_columns: Vec<ColumnDef>,
        retention_duration_ms: Option<u64>,
        max_bytes: Option<u64>,
    ) -> Self {
        // システム擬似列: _offset, _timestamp, _msg_id, _correlation_id
        let mut columns = vec![
            ColumnDef::new("_offset", DataType::BigInt, false, true),
            ColumnDef::new("_timestamp", DataType::TimestampTz, false, false),
            ColumnDef::new("_msg_id", DataType::VarChar(None), true, false),
            ColumnDef::new("_correlation_id", DataType::VarChar(None), true, false),
        ];

        for (idx, col) in columns.iter_mut().enumerate() {
            col.physical_index = Some(idx);
        }

        let base_idx = columns.len();
        for (idx, mut col) in user_columns.into_iter().enumerate() {
            if !col.name.starts_with('_') {
                col.physical_index = Some(base_idx + idx);
                columns.push(col);
            }
        }

        Self {
            name: name.into(),
            schema: default_schema(),
            columns,
            next_row_id: 1,
            foreign_keys: Vec::new(),
            primary_key: vec!["_offset".to_string()],
            unique_constraints: Vec::new(),
            is_queue: true,
            retention_duration_ms,
            max_bytes,
            stats: None,
            approx_row_count: 0,
            is_cache: false,
            cache_ttl_ms: None,
            write_back_table: None,
            write_back_interval_ms: None,
            write_back_mode: None,
            is_unlogged: false,
            is_iceberg: false,
            iceberg_location: None,
        }
    }

    pub fn new_cache(
        name: impl Into<String>,
        user_columns: Vec<ColumnDef>,
        ttl_ms: Option<u64>,
        write_back_table: Option<String>,
        write_back_interval_ms: Option<u64>,
        write_back_mode: Option<String>,
        is_unlogged: bool,
    ) -> Self {
        // システム擬似列: _expires_at, _created_at, _dirty
        let mut columns = vec![
            ColumnDef::new("_expires_at", DataType::BigInt, false, false),
            ColumnDef::new("_created_at", DataType::BigInt, false, false),
            ColumnDef::new("_dirty", DataType::Boolean, false, false),
        ];

        for (idx, col) in columns.iter_mut().enumerate() {
            col.physical_index = Some(idx);
        }

        let base_idx = columns.len();
        let mut pk_cols = Vec::new();
        for (idx, mut col) in user_columns.into_iter().enumerate() {
            if !col.name.starts_with('_') {
                if col.is_primary_key {
                    pk_cols.push(col.name.clone());
                }
                col.physical_index = Some(base_idx + idx);
                columns.push(col);
            }
        }

        Self {
            name: name.into(),
            schema: default_schema(),
            columns,
            next_row_id: 1,
            foreign_keys: Vec::new(),
            primary_key: pk_cols,
            unique_constraints: Vec::new(),
            is_queue: false,
            retention_duration_ms: None,
            max_bytes: None,
            stats: None,
            approx_row_count: 0,
            is_cache: true,
            cache_ttl_ms: ttl_ms,
            write_back_table,
            write_back_interval_ms,
            write_back_mode,
            is_unlogged,
            is_iceberg: false,
            iceberg_location: None,
        }
    }

    pub fn new_iceberg(
        name: impl Into<String>,
        mut columns: Vec<ColumnDef>,
        location: impl Into<String>,
    ) -> Self {
        for (idx, col) in columns.iter_mut().enumerate() {
            if col.physical_index.is_none() {
                col.physical_index = Some(idx);
            }
        }
        let pk_cols: Vec<String> = columns
            .iter()
            .filter(|c| c.is_primary_key)
            .map(|c| c.name.clone())
            .collect();
        Self {
            name: name.into(),
            schema: default_schema(),
            columns,
            next_row_id: 1,
            foreign_keys: Vec::new(),
            primary_key: pk_cols,
            unique_constraints: Vec::new(),
            is_queue: false,
            retention_duration_ms: None,
            max_bytes: None,
            stats: None,
            approx_row_count: 0,
            is_cache: false,
            cache_ttl_ms: None,
            write_back_table: None,
            write_back_interval_ms: None,
            write_back_mode: None,
            is_unlogged: true,
            is_iceberg: true,
            iceberg_location: Some(location.into()),
        }
    }

    pub fn is_iceberg_table(&self) -> bool {
        self.is_iceberg
    }

    pub fn full_name(&self) -> String {
        format!("{}.{}", self.schema.to_lowercase(), self.name.to_lowercase())
    }

    pub fn map_name(&self) -> String {
        let s = self.schema.to_lowercase();
        let n = self.name.to_lowercase();
        if s == "public" {
            format!("tbl_{}", n)
        } else {
            format!("tbl_{}_{}", s, n)
        }
    }

    pub fn get_primary_key(&self) -> Vec<String> {
        if !self.primary_key.is_empty() {
            self.primary_key.clone()
        } else {
            self.columns.iter().filter(|c| c.is_primary_key).map(|c| c.name.clone()).collect()
        }
    }

    pub fn column_index(&self, col_name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name.eq_ignore_ascii_case(col_name))
    }

    /// 行データのカラム構成をテーブル定義の論理カラム列にアライン
    /// （Instant Add Column / Drop Column で生じる物理行と論理定義の差異を透過的に補正）
    pub fn align_row(&self, row: &mut crate::row::Row) {
        let has_explicit_phys = self.columns.iter().any(|c| c.physical_index.is_some());
        if has_explicit_phys {
            let mut new_values = Vec::with_capacity(self.columns.len());
            for col in &self.columns {
                let val = match col.physical_index {
                    Some(phys_idx) if phys_idx < row.values.len() => row.values[phys_idx].clone(),
                    _ => h2_types::Value::Null,
                };
                new_values.push(val);
            }
            row.values = new_values;
        } else if row.values.len() < self.columns.len() {
            row.values.resize(self.columns.len(), h2_types::Value::Null);
        }
    }

    /// 次の物理カラムインデックスを決定
    pub fn next_physical_index(&self) -> usize {
        self.columns
            .iter()
            .filter_map(|c| c.physical_index)
            .max()
            .map(|idx| idx + 1)
            .unwrap_or(self.columns.len())
    }
}

/// 仮想グラフテーブルの種類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualGraphKind {
    Nodes,
    Edges,
}

/// テーブル名から仮想グラフテーブル (graph_<name>_nodes, graph_<name>_edges, graph_<name>.nodes, graph_<name>.edges) を判定
pub fn parse_virtual_graph_table(name: &str) -> Option<(String, VirtualGraphKind)> {
    let lower = name.to_lowercase();
    let trimmed = lower.trim_start_matches("public.");
    let (prefix, suffix) = if let Some(pos) = trimmed.rfind('.') {
        (&trimmed[..pos], &trimmed[pos + 1..])
    } else if let Some(pos) = trimmed.rfind('_') {
        (&trimmed[..pos], &trimmed[pos + 1..])
    } else {
        return None;
    };

    let graph_name = if prefix.starts_with("graph_") {
        prefix[6..].to_string()
    } else if prefix == "graph" {
        "default".to_string()
    } else {
        return None;
    };

    if graph_name.is_empty() {
        return None;
    }

    if suffix == "nodes" {
        Some((graph_name, VirtualGraphKind::Nodes))
    } else if suffix == "edges" {
        Some((graph_name, VirtualGraphKind::Edges))
    } else {
        None
    }
}

/// インデックス定義
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDef {
    pub name: String,
    pub table_name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
}

/// ビュー定義
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewDef {
    pub name: String,
    pub query_sql: String,
    pub columns: Vec<String>,
}

/// カタログ管理
pub struct Catalog {
    store: Arc<MVStore>,
    catalog_map: MVMap,
    tables: Arc<RwLock<HashMap<String, TableDef>>>,
    indexes: Arc<RwLock<HashMap<String, IndexDef>>>,
    views: Arc<RwLock<HashMap<String, ViewDef>>>,
    schemas: Arc<RwLock<HashSet<String>>>,
    sequences: Arc<RwLock<HashMap<String, SequenceDef>>>,
    routines: Arc<RwLock<HashMap<String, RoutineDef>>>,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog")
            .field("tables_count", &self.tables.read().len())
            .field("sequences_count", &self.sequences.read().len())
            .field("routines_count", &self.routines.read().len())
            .finish()
    }
}

impl Catalog {
    pub fn new(store: Arc<MVStore>) -> H2Result<Self> {
        let catalog_map = store.open_map("_catalog");
        let mut tables = HashMap::new();
        let mut indexes = HashMap::new();
        let mut views = HashMap::new();
        let mut schemas = HashSet::new();
        let mut sequences = HashMap::new();
        let mut routines = HashMap::new();
        schemas.insert("public".to_string());

        // 永続化されたカタログ情報をロード
        for entry in catalog_map.scan_all() {
            let key_str = String::from_utf8_lossy(&entry.key).to_string();
            if key_str.starts_with("schema:") {
                let schema_name = &key_str[7..];
                schemas.insert(schema_name.to_lowercase());
            } else if key_str.starts_with("tbl:") {
                let table_name = &key_str[4..];
                if let Ok(table_def) = serde_json::from_slice::<TableDef>(&entry.value) {
                    tables.insert(table_name.to_lowercase(), table_def);
                }
            } else if key_str.starts_with("idx:") {
                let index_name = &key_str[4..];
                if let Ok(index_def) = serde_json::from_slice::<IndexDef>(&entry.value) {
                    indexes.insert(index_name.to_lowercase(), index_def);
                }
            } else if key_str.starts_with("view:") {
                let view_name = &key_str[5..];
                if let Ok(view_def) = serde_json::from_slice::<ViewDef>(&entry.value) {
                    views.insert(view_name.to_lowercase(), view_def);
                }
            } else if key_str.starts_with("seq:") {
                let seq_name = &key_str[4..];
                if let Ok(seq_def) = serde_json::from_slice::<SequenceDef>(&entry.value) {
                    sequences.insert(seq_name.to_lowercase(), seq_def);
                }
            } else if key_str.starts_with("proc:") {
                let proc_name = &key_str[5..];
                if let Ok(routine_def) = serde_json::from_slice::<RoutineDef>(&entry.value) {
                    routines.insert(proc_name.to_lowercase(), routine_def);
                }
            } else {
                // 以前の形式（tbl:プレフィックスなし）との互換性
                if let Ok(table_def) = serde_json::from_slice::<TableDef>(&entry.value) {
                    tables.insert(key_str.to_lowercase(), table_def);
                }
            }
        }

        // 各テーブルの next_row_id を実際の tbl_{name} マップの最大キーより大きく補正
        for table_def in tables.values_mut() {
            let map_name = table_def.map_name();
            let tbl_map = store.open_map(&map_name);
            for entry in tbl_map.scan_all() {
                if entry.key.len() == 8 {
                    if let Ok(k_bytes) = entry.key.as_slice().try_into() {
                        let existing_id = u64::from_le_bytes(k_bytes);
                        if existing_id >= table_def.next_row_id {
                            table_def.next_row_id = existing_id + 1;
                        }
                    }
                }
            }
        }

        Ok(Self {
            store,
            catalog_map,
            tables: Arc::new(RwLock::new(tables)),
            indexes: Arc::new(RwLock::new(indexes)),
            views: Arc::new(RwLock::new(views)),
            schemas: Arc::new(RwLock::new(schemas)),
            sequences: Arc::new(RwLock::new(sequences)),
            routines: Arc::new(RwLock::new(routines)),
        })
    }

    pub fn reload(&self) -> H2Result<()> {
        let mut tables = self.tables.write();
        let mut indexes = self.indexes.write();
        let mut views = self.views.write();
        let mut schemas = self.schemas.write();
        let mut sequences = self.sequences.write();
        let mut routines = self.routines.write();

        tables.clear();
        indexes.clear();
        views.clear();
        schemas.clear();
        sequences.clear();
        routines.clear();
        schemas.insert("public".to_string());

        for entry in self.catalog_map.scan_all() {
            let key_str = String::from_utf8_lossy(&entry.key).to_string();
            if key_str.starts_with("schema:") {
                let schema_name = &key_str[7..];
                schemas.insert(schema_name.to_lowercase());
            } else if key_str.starts_with("tbl:") {
                let table_name = &key_str[4..];
                if let Ok(table_def) = serde_json::from_slice::<TableDef>(&entry.value) {
                    tables.insert(table_name.to_lowercase(), table_def);
                }
            } else if key_str.starts_with("idx:") {
                let index_name = &key_str[4..];
                if let Ok(index_def) = serde_json::from_slice::<IndexDef>(&entry.value) {
                    indexes.insert(index_name.to_lowercase(), index_def);
                }
            } else if key_str.starts_with("view:") {
                let view_name = &key_str[5..];
                if let Ok(view_def) = serde_json::from_slice::<ViewDef>(&entry.value) {
                    views.insert(view_name.to_lowercase(), view_def);
                }
            } else if key_str.starts_with("seq:") {
                let seq_name = &key_str[4..];
                if let Ok(seq_def) = serde_json::from_slice::<SequenceDef>(&entry.value) {
                    sequences.insert(seq_name.to_lowercase(), seq_def);
                }
            } else if key_str.starts_with("proc:") {
                let proc_name = &key_str[5..];
                if let Ok(routine_def) = serde_json::from_slice::<RoutineDef>(&entry.value) {
                    routines.insert(proc_name.to_lowercase(), routine_def);
                }
            } else {
                if let Ok(table_def) = serde_json::from_slice::<TableDef>(&entry.value) {
                    tables.insert(key_str.to_lowercase(), table_def);
                }
            }
        }


        // 各テーブルの next_row_id を実際の tbl_{name} マップの最大キーより大きく補正
        for table_def in tables.values_mut() {
            let map_name = table_def.map_name();
            let tbl_map = self.store.open_map(&map_name);
            for entry in tbl_map.scan_all() {
                if entry.key.len() == 8 {
                    if let Ok(k_bytes) = entry.key.as_slice().try_into() {
                        let existing_id = u64::from_le_bytes(k_bytes);
                        if existing_id >= table_def.next_row_id {
                            table_def.next_row_id = existing_id + 1;
                        }
                    }
                }
            }
        }

        Ok(())
    }


    pub fn store(&self) -> &Arc<MVStore> {
        &self.store
    }

    pub fn create_schema(&self, name: &str) -> H2Result<()> {
        let name_key = name.to_lowercase();
        let mut schemas = self.schemas.write();
        if schemas.contains(&name_key) {
            return Err(H2Error::Catalog(format!("Schema '{}' already exists", name)));
        }
        self.catalog_map.put(format!("schema:{}", name_key).into_bytes(), vec![]);
        schemas.insert(name_key);
        Ok(())
    }

    pub fn drop_schema(&self, name: &str, if_exists: bool, cascade: bool) -> H2Result<Vec<String>> {
        let name_key = name.to_lowercase();
        if name_key == "public" {
            return Err(H2Error::Catalog("Cannot drop public schema".to_string()));
        }
        let mut schemas = self.schemas.write();
        if !schemas.contains(&name_key) {
            if if_exists {
                return Ok(vec![]);
            }
            return Err(H2Error::Catalog(format!("Schema '{}' not found", name)));
        }

        // スキーマ内のテーブルを確認
        let tables_in_schema: Vec<String> = self.tables.read().values()
            .filter(|t| t.schema.eq_ignore_ascii_case(&name_key))
            .map(|t| t.name.clone())
            .collect();

        if !tables_in_schema.is_empty() && !cascade {
            return Err(H2Error::Catalog(format!(
                "Cannot drop schema '{}' because other objects depend on it (use CASCADE)",
                name
            )));
        }

        let mut dropped_maps = Vec::new();
        if cascade {
            for tbl_name in tables_in_schema {
                let full = format!("{}.{}", name_key, tbl_name.to_lowercase());
                if let Ok(maps) = self.drop_table(&full) {
                    dropped_maps.extend(maps);
                }
            }
        }

        schemas.remove(&name_key);
        self.catalog_map.remove(format!("schema:{}", name_key).as_bytes());
        Ok(dropped_maps)
    }

    pub fn get_schemas(&self) -> Vec<String> {
        self.schemas.read().iter().cloned().collect()
    }

    pub fn schema_exists(&self, name: &str) -> bool {
        self.schemas.read().contains(&name.to_lowercase())
    }

    pub fn create_table(&self, table_def: TableDef) -> H2Result<()> {
        let schema_key = table_def.schema.to_lowercase();
        if !self.schemas.read().contains(&schema_key) {
            return Err(H2Error::Catalog(format!("Schema '{}' does not exist", table_def.schema)));
        }

        let full_key = table_def.full_name();
        let name_key = table_def.name.to_lowercase();

        if self.views.read().contains_key(&full_key) || self.views.read().contains_key(&name_key) {
            return Err(H2Error::Catalog(format!(
                "Cannot create table '{}': view with same name exists",
                table_def.name
            )));
        }
        let mut tables = self.tables.write();

        if tables.contains_key(&full_key) || (schema_key == "public" && tables.contains_key(&name_key)) {
            return Err(H2Error::Catalog(format!(
                "Table '{}' already exists",
                table_def.name
            )));
        }

        let serialized = serde_json::to_vec(&table_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("tbl:{}", full_key).into_bytes(), serialized.clone());
        if schema_key == "public" {
            self.catalog_map.put(format!("tbl:{}", name_key).into_bytes(), serialized);
            tables.insert(name_key, table_def.clone());
        }
        tables.insert(full_key, table_def);

        Ok(())
    }

    pub fn get_table(&self, name: &str) -> Option<TableDef> {
        let name_key = name.to_lowercase();
        let tables = self.tables.read();
        if let Some(t) = tables.get(&name_key) {
            return Some(t.clone());
        }
        if !name_key.contains('.') {
            let pub_key = format!("public.{}", name_key);
            if let Some(t) = tables.get(&pub_key) {
                return Some(t.clone());
            }
        } else {
            let parts: Vec<&str> = name_key.splitn(2, '.').collect();
            if parts[0] == "public" {
                if let Some(t) = tables.get(parts[1]) {
                    return Some(t.clone());
                }
            }
        }

        if let Some((_graph_name, kind)) = parse_virtual_graph_table(&name_key) {
            let cols = match kind {
                VirtualGraphKind::Nodes => vec![
                    ColumnDef::new("id", DataType::BigInt, false, true),
                    ColumnDef::new("labels", DataType::Array(Box::new(DataType::VarChar(None))), false, false),
                    ColumnDef::new("properties", DataType::Json, false, false),
                ],
                VirtualGraphKind::Edges => vec![
                    ColumnDef::new("id", DataType::BigInt, false, true),
                    ColumnDef::new("src_id", DataType::BigInt, false, false),
                    ColumnDef::new("dst_id", DataType::BigInt, false, false),
                    ColumnDef::new("type", DataType::VarChar(None), false, false),
                    ColumnDef::new("properties", DataType::Json, false, false),
                ],
            };
            return Some(TableDef::new(name, cols));
        }

        None
    }

    pub fn is_queue_table(&self, name: &str) -> bool {
        self.get_table(name).map(|t| t.is_queue).unwrap_or(false)
    }

    pub fn is_cache_table(&self, name: &str) -> bool {
        self.get_table(name).map(|t| t.is_cache).unwrap_or(false)
    }

    pub fn all_tables(&self) -> Vec<TableDef> {
        let tables = self.tables.read();
        let mut seen = std::collections::HashSet::new();
        let mut list = Vec::new();
        for t in tables.values() {
            let full = t.full_name();
            if !seen.contains(&full) {
                seen.insert(full);
                list.push(t.clone());
            }
        }
        list
    }

    pub fn get_tables_referencing(&self, parent_table: &str) -> Vec<(TableDef, ForeignKeyDef)> {
        let parent_lower = parent_table.to_lowercase();
        let mut res = Vec::new();
        for table in self.tables.read().values() {
            for fk in &table.foreign_keys {
                if fk.foreign_table.to_lowercase() == parent_lower {
                    res.push((table.clone(), fk.clone()));
                }
            }
        }
        res
    }

    pub fn drop_table(&self, name: &str) -> H2Result<Vec<String>> {
        let name_key = name.to_lowercase();
        let mut tables = self.tables.write();
        let table_def = tables.remove(&name_key).or_else(|| {
            if !name_key.contains('.') {
                tables.remove(&format!("public.{}", name_key))
            } else {
                let parts: Vec<&str> = name_key.splitn(2, '.').collect();
                if parts[0] == "public" {
                    tables.remove(parts[1])
                } else {
                    None
                }
            }
        }).ok_or_else(|| H2Error::Catalog(format!("Table '{}' not found", name)))?;

        let full_key = table_def.full_name();
        let simple_key = table_def.name.to_lowercase();
        tables.remove(&full_key);
        tables.remove(&simple_key);

        self.catalog_map.remove(format!("tbl:{}", full_key).as_bytes());
        self.catalog_map.remove(format!("tbl:{}", simple_key).as_bytes());

        let mut dropped_maps = vec![table_def.map_name()];

        let mut indexes = self.indexes.write();
        let idx_keys_to_remove: Vec<String> = indexes
            .iter()
            .filter(|(_, idx)| {
                idx.table_name.eq_ignore_ascii_case(&full_key)
                    || idx.table_name.eq_ignore_ascii_case(&simple_key)
            })
            .map(|(k, _)| k.clone())
            .collect();

        for idx_key in idx_keys_to_remove {
            if let Some(idx_def) = indexes.remove(&idx_key) {
                self.catalog_map.remove(format!("idx:{}", idx_key).as_bytes());
                dropped_maps.push(format!("idx_{}_{}", simple_key, idx_def.name.to_lowercase()));
            }
        }

        Ok(dropped_maps)
    }

    pub fn update_table(&self, table_def: TableDef) -> H2Result<()> {
        let name_key = table_def.name.to_lowercase();
        let full_key = table_def.full_name();
        let mut tables = self.tables.write();
        if !tables.contains_key(&name_key) && !tables.contains_key(&full_key) {
            return Err(H2Error::Catalog(format!("Table '{}' not found", table_def.name)));
        }

        let serialized = serde_json::to_vec(&table_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("tbl:{}", full_key).into_bytes(), serialized.clone());
        tables.insert(full_key, table_def.clone());
        if table_def.schema.eq_ignore_ascii_case("public") {
            self.catalog_map.put(format!("tbl:{}", name_key).into_bytes(), serialized);
            tables.insert(name_key, table_def);
        }

        Ok(())
    }

    pub fn update_table_stats(
        &self,
        table_name: &str,
        stats: TableStats,
        column_stats: HashMap<String, ColumnStats>,
    ) -> H2Result<()> {
        let mut table = self.get_table(table_name)
            .ok_or_else(|| H2Error::Catalog(format!("Table '{}' not found", table_name)))?;
        table.approx_row_count = stats.row_count as i64;
        table.stats = Some(stats);
        for col in &mut table.columns {
            if let Some(cs) = column_stats.get(&col.name.to_lowercase()) {
                col.stats = Some(cs.clone());
            }
        }
        self.update_table(table)
    }

    pub fn update_approx_row_count(&self, table_name: &str, delta: i64) -> H2Result<()> {
        let mut table = match self.get_table(table_name) {
            Some(t) => t,
            None => return Ok(()),
        };
        table.approx_row_count = (table.approx_row_count + delta).max(0);
        if let Some(ref mut s) = table.stats {
            s.row_count = table.approx_row_count as u64;
        }
        self.update_table(table)
    }

    pub fn reset_approx_row_count(&self, table_name: &str, count: i64) -> H2Result<()> {
        let mut table = match self.get_table(table_name) {
            Some(t) => t,
            None => return Ok(()),
        };
        table.approx_row_count = count.max(0);
        if let Some(ref mut s) = table.stats {
            s.row_count = table.approx_row_count as u64;
        }
        self.update_table(table)
    }

    pub fn rename_table(&self, old_name: &str, new_name: &str) -> H2Result<()> {
        let old_key = old_name.to_lowercase();
        let mut tables = self.tables.write();

        let mut table_def = tables.remove(&old_key).or_else(|| {
            if !old_key.contains('.') {
                tables.remove(&format!("public.{}", old_key))
            } else {
                let parts: Vec<&str> = old_key.splitn(2, '.').collect();
                if parts[0] == "public" {
                    tables.remove(parts[1])
                } else {
                    None
                }
            }
        }).ok_or_else(|| {
            H2Error::Catalog(format!("Table '{}' not found", old_name))
        })?;

        let old_full = table_def.full_name();
        let old_simple = table_def.name.to_lowercase();
        tables.remove(&old_full);
        tables.remove(&old_simple);
        self.catalog_map.remove(format!("tbl:{}", old_full).as_bytes());
        self.catalog_map.remove(format!("tbl:{}", old_simple).as_bytes());

        let (new_schema, new_simple) = if new_name.contains('.') {
            let parts: Vec<&str> = new_name.splitn(2, '.').collect();
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (table_def.schema.clone(), new_name.to_string())
        };

        let new_full = format!("{}.{}", new_schema.to_lowercase(), new_simple.to_lowercase());
        let new_simple_key = new_simple.to_lowercase();

        if tables.contains_key(&new_full) || (new_schema.eq_ignore_ascii_case("public") && tables.contains_key(&new_simple_key)) {
            return Err(H2Error::Catalog(format!("Table '{}' already exists", new_name)));
        }

        table_def.schema = new_schema.clone();
        table_def.name = new_simple;

        let serialized = serde_json::to_vec(&table_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("tbl:{}", new_full).into_bytes(), serialized.clone());
        if new_schema.eq_ignore_ascii_case("public") {
            self.catalog_map.put(format!("tbl:{}", new_simple_key).into_bytes(), serialized);
            tables.insert(new_simple_key.clone(), table_def.clone());
        }
        tables.insert(new_full.clone(), table_def);

        // 関連インデックスの table_name も更新
        let mut indexes = self.indexes.write();
        for (_, idx) in indexes.iter_mut() {
            if idx.table_name.eq_ignore_ascii_case(&old_full) || idx.table_name.eq_ignore_ascii_case(&old_simple) {
                idx.table_name = if new_schema.eq_ignore_ascii_case("public") {
                    new_simple_key.clone()
                } else {
                    new_full.clone()
                };
                let serialized_idx = serde_json::to_vec(&*idx)
                    .map_err(|e| H2Error::Serialization(e.to_string()))?;
                self.catalog_map.put(format!("idx:{}", idx.name.to_lowercase()).into_bytes(), serialized_idx);
            }
        }

        Ok(())
    }

    pub fn create_index(&self, index_def: IndexDef) -> H2Result<()> {
        let name_key = index_def.name.to_lowercase();
        let mut indexes = self.indexes.write();

        if indexes.contains_key(&name_key) {
            return Err(H2Error::Catalog(format!(
                "Index '{}' already exists",
                index_def.name
            )));
        }

        let serialized = serde_json::to_vec(&index_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("idx:{}", name_key).into_bytes(), serialized);
        indexes.insert(name_key, index_def);

        Ok(())
    }

    pub fn get_index(&self, name: &str) -> Option<IndexDef> {
        let name_key = name.to_lowercase();
        self.indexes.read().get(&name_key).cloned()
    }

    pub fn get_table_indexes(&self, table_name: &str) -> Vec<IndexDef> {
        let table_key = table_name.to_lowercase();
        let short_key = if let Some((_, t)) = table_key.split_once('.') {
            t.to_string()
        } else {
            table_key.clone()
        };
        let full_key = if !table_key.contains('.') {
            format!("public.{}", table_key)
        } else {
            table_key.clone()
        };
        self.indexes
            .read()
            .values()
            .filter(|idx| {
                let lower = idx.table_name.to_lowercase();
                lower == table_key || lower == short_key || lower == full_key
            })
            .cloned()
            .collect()
    }

    pub fn all_indexes(&self) -> Vec<IndexDef> {
        self.indexes.read().values().cloned().collect()
    }

    pub fn drop_index(&self, name: &str) -> H2Result<String> {
        let name_key = name.to_lowercase();
        let mut indexes = self.indexes.write();
        let idx_def = indexes.remove(&name_key).ok_or_else(|| {
            H2Error::Catalog(format!("Index '{}' not found", name))
        })?;

        self.catalog_map.remove(format!("idx:{}", name_key).as_bytes());
        Ok(format!("idx_{}_{}", idx_def.table_name.to_lowercase(), idx_def.name.to_lowercase()))
    }

    pub fn allocate_row_id(&self, table_name: &str) -> H2Result<u64> {
        let name_key = table_name.to_lowercase();
        let mut tables = self.tables.write();
        let target_key = if tables.contains_key(&name_key) {
            name_key
        } else if !name_key.contains('.') && tables.contains_key(&format!("public.{}", name_key)) {
            format!("public.{}", name_key)
        } else if let Some((first, rest)) = name_key.split_once('.') {
            if first == "public" && tables.contains_key(rest) {
                rest.to_string()
            } else {
                name_key
            }
        } else {
            name_key
        };

        let table_def = tables.get_mut(&target_key).ok_or_else(|| {
            H2Error::Catalog(format!("Table '{}' not found", table_name))
        })?;

        let id = table_def.next_row_id;
        table_def.next_row_id += 1;

        let serialized = serde_json::to_vec(&table_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("tbl:{}", table_def.full_name()).into_bytes(), serialized.clone());
        if table_def.schema.eq_ignore_ascii_case("public") {
            self.catalog_map.put(format!("tbl:{}", table_def.name.to_lowercase()).into_bytes(), serialized);
        }

        Ok(id)
    }

    // ================= SEQUENCE 管理 =================

    pub fn create_sequence(&self, seq_def: SequenceDef, if_not_exists: bool) -> H2Result<()> {
        let full_key = seq_def.full_name();
        let simple_key = seq_def.name.to_lowercase();
        let mut sequences = self.sequences.write();

        if sequences.contains_key(&full_key) || (seq_def.schema == "public" && sequences.contains_key(&simple_key)) {
            if if_not_exists {
                return Ok(());
            }
            return Err(H2Error::Catalog(format!("Sequence '{}' already exists", seq_def.name)));
        }

        let serialized = serde_json::to_vec(&seq_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("seq:{}", full_key).into_bytes(), serialized.clone());
        if seq_def.schema == "public" {
            self.catalog_map.put(format!("seq:{}", simple_key).into_bytes(), serialized);
            sequences.insert(simple_key, seq_def.clone());
        }
        sequences.insert(full_key, seq_def);

        Ok(())
    }

    pub fn drop_sequence(&self, name: &str, if_exists: bool) -> H2Result<()> {
        let name_key = name.to_lowercase();
        let mut sequences = self.sequences.write();
        let seq_def = sequences.remove(&name_key).or_else(|| {
            if !name_key.contains('.') {
                sequences.remove(&format!("public.{}", name_key))
            } else {
                let parts: Vec<&str> = name_key.splitn(2, '.').collect();
                if parts[0] == "public" {
                    sequences.remove(parts[1])
                } else {
                    None
                }
            }
        });

        if let Some(s) = seq_def {
            let full_key = s.full_name();
            let simple_key = s.name.to_lowercase();
            sequences.remove(&full_key);
            sequences.remove(&simple_key);
            self.catalog_map.remove(format!("seq:{}", full_key).as_bytes());
            self.catalog_map.remove(format!("seq:{}", simple_key).as_bytes());
            Ok(())
        } else if if_exists {
            Ok(())
        } else {
            Err(H2Error::Catalog(format!("Sequence '{}' not found", name)))
        }
    }

    pub fn alter_sequence(
        &self,
        name: &str,
        restart: Option<i64>,
        increment: Option<i64>,
        min_value: Option<i64>,
        max_value: Option<i64>,
        cycle: Option<bool>,
    ) -> H2Result<()> {
        let name_key = name.to_lowercase();
        let mut sequences = self.sequences.write();
        let key = if sequences.contains_key(&name_key) {
            name_key
        } else if !name_key.contains('.') && sequences.contains_key(&format!("public.{}", name_key)) {
            format!("public.{}", name_key)
        } else {
            return Err(H2Error::Catalog(format!("Sequence '{}' not found", name)));
        };

        let seq_def = sequences.get_mut(&key).unwrap();
        if let Some(r) = restart {
            seq_def.current_value = r;
            seq_def.is_called = false;
        }
        if let Some(inc) = increment {
            seq_def.increment_by = inc;
        }
        if let Some(min_v) = min_value {
            seq_def.min_value = min_v;
        }
        if let Some(max_v) = max_value {
            seq_def.max_value = max_v;
        }
        if let Some(cyc) = cycle {
            seq_def.cycle = cyc;
        }

        let serialized = serde_json::to_vec(&*seq_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("seq:{}", seq_def.full_name()).into_bytes(), serialized.clone());
        if seq_def.schema == "public" {
            self.catalog_map.put(format!("seq:{}", seq_def.name.to_lowercase()).into_bytes(), serialized);
        }
        Ok(())
    }

    pub fn nextval(&self, name: &str) -> H2Result<i64> {
        let name_key = name.to_lowercase();
        let mut sequences = self.sequences.write();
        let key = if sequences.contains_key(&name_key) {
            name_key
        } else if !name_key.contains('.') && sequences.contains_key(&format!("public.{}", name_key)) {
            format!("public.{}", name_key)
        } else {
            return Err(H2Error::Catalog(format!("Sequence '{}' not found", name)));
        };

        let seq = sequences.get_mut(&key).unwrap();
        let val = if !seq.is_called {
            seq.is_called = true;
            seq.current_value
        } else {
            let next = seq.current_value.saturating_add(seq.increment_by);
            if seq.increment_by > 0 && next > seq.max_value {
                if seq.cycle {
                    seq.min_value
                } else {
                    return Err(H2Error::Execution(format!("Sequence '{}' reached maximum value", name)));
                }
            } else if seq.increment_by < 0 && next < seq.min_value {
                if seq.cycle {
                    seq.max_value
                } else {
                    return Err(H2Error::Execution(format!("Sequence '{}' reached minimum value", name)));
                }
            } else {
                next
            }
        };

        seq.current_value = val;
        let serialized = serde_json::to_vec(&*seq)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        let full = seq.full_name();
        let simple = seq.name.to_lowercase();
        let is_public = seq.schema == "public";
        self.catalog_map.put(format!("seq:{}", full).into_bytes(), serialized.clone());
        if is_public {
            self.catalog_map.put(format!("seq:{}", simple).into_bytes(), serialized);
        }

        Ok(val)
    }

    pub fn currval(&self, name: &str) -> H2Result<i64> {
        let name_key = name.to_lowercase();
        let sequences = self.sequences.read();
        let seq = sequences.get(&name_key).or_else(|| {
            if !name_key.contains('.') {
                sequences.get(&format!("public.{}", name_key))
            } else {
                None
            }
        }).ok_or_else(|| H2Error::Catalog(format!("Sequence '{}' not found", name)))?;

        if !seq.is_called {
            return Err(H2Error::Execution(format!("currval of sequence '{}' is not yet defined in this session", name)));
        }
        Ok(seq.current_value)
    }

    pub fn setval(&self, name: &str, val: i64, is_called: bool) -> H2Result<i64> {
        let name_key = name.to_lowercase();
        let mut sequences = self.sequences.write();
        let key = if sequences.contains_key(&name_key) {
            name_key
        } else if !name_key.contains('.') && sequences.contains_key(&format!("public.{}", name_key)) {
            format!("public.{}", name_key)
        } else {
            return Err(H2Error::Catalog(format!("Sequence '{}' not found", name)));
        };

        let seq = sequences.get_mut(&key).unwrap();
        seq.current_value = val;
        seq.is_called = is_called;

        let serialized = serde_json::to_vec(&*seq)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        let full = seq.full_name();
        let simple = seq.name.to_lowercase();
        let is_public = seq.schema == "public";
        self.catalog_map.put(format!("seq:{}", full).into_bytes(), serialized.clone());
        if is_public {
            self.catalog_map.put(format!("seq:{}", simple).into_bytes(), serialized);
        }

        Ok(val)
    }

    pub fn get_sequence(&self, name: &str) -> Option<SequenceDef> {
        let name_key = name.to_lowercase();
        let sequences = self.sequences.read();
        sequences.get(&name_key).cloned().or_else(|| {
            if !name_key.contains('.') {
                sequences.get(&format!("public.{}", name_key)).cloned()
            } else {
                None
            }
        })
    }

    pub fn all_sequences(&self) -> Vec<SequenceDef> {
        let sequences = self.sequences.read();
        let mut seen = HashSet::new();
        let mut list = Vec::new();
        for s in sequences.values() {
            let full = s.full_name();
            if !seen.contains(&full) {
                seen.insert(full);
                list.push(s.clone());
            }
        }
        list
    }

    pub fn create_view(&self, view_def: ViewDef, or_replace: bool) -> H2Result<()> {
        let name_key = view_def.name.to_lowercase();
        if self.tables.read().contains_key(&name_key) {
            return Err(H2Error::Catalog(format!(
                "Cannot create view '{}': table with same name exists",
                view_def.name
            )));
        }
        let mut views = self.views.write();
        if views.contains_key(&name_key) && !or_replace {
            return Err(H2Error::Catalog(format!(
                "View '{}' already exists",
                view_def.name
            )));
        }

        let serialized = serde_json::to_vec(&view_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("view:{}", name_key).into_bytes(), serialized);
        views.insert(name_key, view_def);

        Ok(())
    }

    pub fn drop_view(&self, name: &str, if_exists: bool) -> H2Result<()> {
        let name_key = name.to_lowercase();
        let mut views = self.views.write();
        if views.remove(&name_key).is_none() {
            if if_exists {
                return Ok(());
            }
            return Err(H2Error::Catalog(format!("View '{}' not found", name)));
        }

        self.catalog_map.remove(format!("view:{}", name_key).as_bytes());
        Ok(())
    }

    pub fn get_view(&self, name: &str) -> Option<ViewDef> {
        let name_key = name.to_lowercase();
        self.views.read().get(&name_key).cloned()
    }

    pub fn all_views(&self) -> Vec<ViewDef> {
        self.views.read().values().cloned().collect()
    }

    pub fn create_routine(&self, routine_def: RoutineDef) -> H2Result<()> {
        let name_key = routine_def.name.to_lowercase();
        let serialized = serde_json::to_vec(&routine_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("proc:{}", name_key).into_bytes(), serialized);
        let _ = self.store.commit();
        self.routines.write().insert(name_key, routine_def);
        Ok(())
    }

    pub fn drop_routine(&self, name: &str, if_exists: bool) -> H2Result<()> {
        let name_key = name.to_lowercase();
        let mut routines = self.routines.write();
        if routines.remove(&name_key).is_none() {
            if if_exists {
                return Ok(());
            }
            return Err(H2Error::Catalog(format!("Routine '{}' not found", name)));
        }

        self.catalog_map.remove(format!("proc:{}", name_key).as_bytes());
        let _ = self.store.commit();
        Ok(())
    }

    pub fn get_routine(&self, name: &str) -> Option<RoutineDef> {
        let name_key = name.to_lowercase();
        self.routines.read().get(&name_key).cloned()
    }

    pub fn all_routines(&self) -> Vec<RoutineDef> {
        self.routines.read().values().cloned().collect()
    }
}

