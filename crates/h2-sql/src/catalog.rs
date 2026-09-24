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

/// テーブル定義
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDef {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub next_row_id: u64,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKeyDef>,
}

impl TableDef {
    pub fn new(name: impl Into<String>, columns: Vec<ColumnDef>) -> Self {
        Self {
            name: name.into(),
            columns,
            next_row_id: 1,
            foreign_keys: Vec::new(),
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
}

impl Catalog {
    pub fn new(store: Arc<MVStore>) -> H2Result<Self> {
        let catalog_map = store.open_map("_catalog");
        let mut tables = HashMap::new();
        let mut indexes = HashMap::new();
        let mut views = HashMap::new();

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
            } else if key_str.starts_with("view:") {
                let view_name = &key_str[5..];
                if let Ok(view_def) = serde_json::from_slice::<ViewDef>(&entry.value) {
                    views.insert(view_name.to_lowercase(), view_def);
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
            views: Arc::new(RwLock::new(views)),
        })
    }


    pub fn store(&self) -> &Arc<MVStore> {
        &self.store
    }

    pub fn create_table(&self, table_def: TableDef) -> H2Result<()> {
        let name_key = table_def.name.to_lowercase();
        if self.views.read().contains_key(&name_key) {
            return Err(H2Error::Catalog(format!(
                "Cannot create table '{}': view with same name exists",
                table_def.name
            )));
        }
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
        if !tables.contains_key(&name_key) {
            return Err(H2Error::Catalog(format!("Table '{}' not found", name)));
        }

        tables.remove(&name_key);
        self.catalog_map.remove(format!("tbl:{}", name_key).as_bytes());

        let mut dropped_maps = vec![format!("tbl_{}", name_key)];

        let mut indexes = self.indexes.write();
        let idx_keys_to_remove: Vec<String> = indexes
            .iter()
            .filter(|(_, idx)| idx.table_name.eq_ignore_ascii_case(&name_key))
            .map(|(k, _)| k.clone())
            .collect();

        for idx_key in idx_keys_to_remove {
            if let Some(idx_def) = indexes.remove(&idx_key) {
                self.catalog_map.remove(format!("idx:{}", idx_key).as_bytes());
                dropped_maps.push(format!("idx_{}_{}", name_key, idx_def.name.to_lowercase()));
            }
        }

        Ok(dropped_maps)
    }

    pub fn update_table(&self, table_def: TableDef) -> H2Result<()> {
        let name_key = table_def.name.to_lowercase();
        let mut tables = self.tables.write();
        if !tables.contains_key(&name_key) {
            return Err(H2Error::Catalog(format!("Table '{}' not found", table_def.name)));
        }

        let serialized = serde_json::to_vec(&table_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("tbl:{}", name_key).into_bytes(), serialized);
        tables.insert(name_key, table_def);

        Ok(())
    }

    pub fn rename_table(&self, old_name: &str, new_name: &str) -> H2Result<()> {
        let old_key = old_name.to_lowercase();
        let new_key = new_name.to_lowercase();
        let mut tables = self.tables.write();

        let mut table_def = tables.remove(&old_key).ok_or_else(|| {
            H2Error::Catalog(format!("Table '{}' not found", old_name))
        })?;

        if tables.contains_key(&new_key) {
            return Err(H2Error::Catalog(format!("Table '{}' already exists", new_name)));
        }

        self.catalog_map.remove(format!("tbl:{}", old_key).as_bytes());
        table_def.name = new_name.to_string();

        let serialized = serde_json::to_vec(&table_def)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;
        self.catalog_map.put(format!("tbl:{}", new_key).into_bytes(), serialized);
        tables.insert(new_key.clone(), table_def);

        // 関連インデックスの table_name も更新
        let mut indexes = self.indexes.write();
        for (_, idx) in indexes.iter_mut() {
            if idx.table_name.eq_ignore_ascii_case(&old_key) {
                idx.table_name = new_name.to_string();
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
}

