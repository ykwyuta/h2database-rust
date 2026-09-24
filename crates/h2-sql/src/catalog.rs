use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;

use h2_mvstore::{MVMap, MVStore};
use h2_types::{DataType, H2Error, H2Result};

/// カラム定義
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub is_nullable: bool,
    pub is_primary_key: bool,
}

/// テーブル定義
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDef {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub next_row_id: u64,
}

impl TableDef {
    pub fn new(name: impl Into<String>, columns: Vec<ColumnDef>) -> Self {
        Self {
            name: name.into(),
            columns,
            next_row_id: 1,
        }
    }

    pub fn column_index(&self, col_name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name.eq_ignore_ascii_case(col_name))
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

/// カタログ管理
pub struct Catalog {
    store: Arc<MVStore>,
    catalog_map: MVMap,
    tables: Arc<RwLock<HashMap<String, TableDef>>>,
    indexes: Arc<RwLock<HashMap<String, IndexDef>>>,
}

impl Catalog {
    pub fn new(store: Arc<MVStore>) -> H2Result<Self> {
        let catalog_map = store.open_map("_catalog");
        let mut tables = HashMap::new();
        let mut indexes = HashMap::new();

        // 永続化されたカタログ情報をロード
        for entry in catalog_map.scan_all() {
            let key_str = String::from_utf8_lossy(&entry.key).to_string();
            if key_str.starts_with("tbl:") {
                let table_name = &key_str[4..];
                if let Ok(table_def) = serde_json::from_slice::<TableDef>(&entry.value) {
                    tables.insert(table_name.to_lowercase(), table_def);
                }
            } else if key_str.starts_with("idx:") {
                let index_name = &key_str[4..];
                if let Ok(index_def) = serde_json::from_slice::<IndexDef>(&entry.value) {
                    indexes.insert(index_name.to_lowercase(), index_def);
                }
            } else {
                // 以前の形式（tbl:プレフィックスなし）との互換性
                if let Ok(table_def) = serde_json::from_slice::<TableDef>(&entry.value) {
                    tables.insert(key_str.to_lowercase(), table_def);
                }
            }
        }

        Ok(Self {
            store,
            catalog_map,
            tables: Arc::new(RwLock::new(tables)),
            indexes: Arc::new(RwLock::new(indexes)),
        })
    }


    pub fn store(&self) -> &Arc<MVStore> {
        &self.store
    }

    pub fn create_table(&self, table_def: TableDef) -> H2Result<()> {
        let name_key = table_def.name.to_lowercase();
        let mut tables = self.tables.write();

        if tables.contains_key(&name_key) {
            return Err(H2Error::Catalog(format!(
                "Table '{}' already exists",
                table_def.name
            )));
        }

        let serialized = serde_json::to_vec(&table_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("tbl:{}", name_key).into_bytes(), serialized);
        tables.insert(name_key, table_def);

        Ok(())
    }

    pub fn get_table(&self, name: &str) -> Option<TableDef> {
        let name_key = name.to_lowercase();
        self.tables.read().get(&name_key).cloned()
    }

    pub fn all_tables(&self) -> Vec<TableDef> {
        self.tables.read().values().cloned().collect()
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
        self.indexes
            .read()
            .values()
            .filter(|idx| idx.table_name.eq_ignore_ascii_case(&table_key))
            .cloned()
            .collect()
    }

    pub fn all_indexes(&self) -> Vec<IndexDef> {
        self.indexes.read().values().cloned().collect()
    }

    pub fn allocate_row_id(&self, table_name: &str) -> H2Result<u64> {
        let name_key = table_name.to_lowercase();
        let mut tables = self.tables.write();
        let table_def = tables.get_mut(&name_key).ok_or_else(|| {
            H2Error::Catalog(format!("Table '{}' not found", table_name))
        })?;

        let id = table_def.next_row_id;
        table_def.next_row_id += 1;

        let serialized = serde_json::to_vec(&table_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("tbl:{}", name_key).into_bytes(), serialized);

        Ok(id)
    }
}

