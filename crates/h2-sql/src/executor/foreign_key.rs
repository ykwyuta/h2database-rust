use h2_mvstore::Transaction;
use h2_types::{H2Error, H2Result, Value};
use crate::catalog::{ForeignKeyAction, TableDef};
use crate::row::Row;
use super::*;

impl SQLEngine {
    pub(crate) fn validate_foreign_keys_for_row(
        &self,
        tx: &Transaction,
        table_def: &TableDef,
        row: &Row,
    ) -> H2Result<()> {
        for fk in &table_def.foreign_keys {
            let col_idx = match table_def.column_index(&fk.column) {
                Some(i) => i,
                None => continue,
            };
            let val = match row.values.get(col_idx) {
                Some(v) => v,
                None => continue,
            };
            if val.is_null() {
                // SQL標準: NULL は親に一致しなくてもよい
                continue;
            }

            let parent_table_def = self.catalog.get_table(&fk.foreign_table).ok_or_else(|| {
                H2Error::Execution(format!("Referenced parent table '{}' not found", fk.foreign_table))
            })?;
            let parent_col_idx = parent_table_def.column_index(&fk.foreign_column).ok_or_else(|| {
                H2Error::Execution(format!(
                    "Referenced column '{}' not found in parent table '{}'",
                    fk.foreign_column, fk.foreign_table
                ))
            })?;

            let parent_map = format!("tbl_{}", fk.foreign_table.to_lowercase());
            let entries = tx.scan_visible(&parent_map)?;
            let mut exists = false;
            for (_k, v) in entries {
                let mut p_row = Row::from_bytes(&v)?;
                parent_table_def.align_row(&mut p_row);
                if p_row.values.get(parent_col_idx) == Some(val) {
                    exists = true;
                    break;
                }
            }

            if !exists {
                return Err(H2Error::Execution(format!(
                    "Foreign key constraint violation: value {:?} in column '{}' of table '{}' does not exist in parent table '{}.{}'",
                    val, fk.column, table_def.name, fk.foreign_table, fk.foreign_column
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn handle_foreign_keys_on_delete(
        &self,
        tx: &Transaction,
        parent_table: &str,
        parent_row: &Row,
    ) -> H2Result<()> {
        let parent_table_def = match self.catalog.get_table(parent_table) {
            Some(t) => t,
            None => return Ok(()),
        };

        let referencing = self.catalog.get_tables_referencing(parent_table);
        for (child_table_def, fk) in referencing {
            let parent_col_idx = match parent_table_def.column_index(&fk.foreign_column) {
                Some(i) => i,
                None => continue,
            };
            let parent_val = match parent_row.values.get(parent_col_idx) {
                Some(v) => v,
                None => continue,
            };
            if parent_val.is_null() {
                continue;
            }

            let child_col_idx = match child_table_def.column_index(&fk.column) {
                Some(i) => i,
                None => continue,
            };

            let child_map_name = format!("tbl_{}", child_table_def.name.to_lowercase());
            let child_entries = tx.scan_visible(&child_map_name)?;
            let mut matching_child_rows = Vec::new();
            for (c_key, c_val_bytes) in child_entries {
                let mut c_row = Row::from_bytes(&c_val_bytes)?;
                child_table_def.align_row(&mut c_row);
                if c_row.values.get(child_col_idx) == Some(parent_val) {
                    matching_child_rows.push((c_key, c_row));
                }
            }

            if matching_child_rows.is_empty() {
                continue;
            }

            match fk.on_delete {
                ForeignKeyAction::Restrict | ForeignKeyAction::NoAction => {
                    return Err(H2Error::Execution(format!(
                        "Foreign key constraint violation: cannot delete from table '{}' because record is referenced by table '{}' (foreign key on column '{}')",
                        parent_table, child_table_def.name, fk.column
                    )));
                }
                ForeignKeyAction::Cascade => {
                    let child_indexes = self.catalog.get_table_indexes(&child_table_def.name);
                    for (c_key, c_row) in matching_child_rows {
                        let c_row_id = if c_key.len() == 8 {
                            u64::from_le_bytes(c_key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };

                        // 再帰的に孫テーブル等の外部キー連動処理
                        self.handle_foreign_keys_on_delete(tx, &child_table_def.name, &c_row)?;

                        // インデックスから削除
                        for idx in &child_indexes {
                            if let Some(ci) = child_table_def.column_index(&idx.columns[0]) {
                                let col_val = &c_row.values[ci];
                                let idx_map = format!("idx_{}_{}", child_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                let idx_key = encode_index_key(col_val, c_row_id);
                                tx.remove(&idx_map, &idx_key)?;
                            }
                        }

                        tx.remove(&child_map_name, &c_key)?;
                    }
                }
                ForeignKeyAction::SetNull => {
                    let child_indexes = self.catalog.get_table_indexes(&child_table_def.name);
                    for (c_key, mut c_row) in matching_child_rows {
                        let c_row_id = if c_key.len() == 8 {
                            u64::from_le_bytes(c_key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };
                        let old_val = c_row.values[child_col_idx].clone();
                        c_row.values[child_col_idx] = Value::Null;

                        // インデックス更新
                        for idx in &child_indexes {
                            if let Some(ci) = child_table_def.column_index(&idx.columns[0]) {
                                if ci == child_col_idx {
                                    let idx_map = format!("idx_{}_{}", child_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                    let old_idx_key = encode_index_key(&old_val, c_row_id);
                                    tx.remove(&idx_map, &old_idx_key)?;
                                    let new_idx_key = encode_index_key(&Value::Null, c_row_id);
                                    tx.put(&idx_map, new_idx_key, vec![])?;
                                }
                            }
                        }

                        tx.put(&child_map_name, c_key, c_row.to_bytes()?)?;
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) fn handle_foreign_keys_on_update(
        &self,
        tx: &Transaction,
        parent_table: &str,
        old_parent_row: &Row,
        new_parent_row: &Row,
    ) -> H2Result<()> {
        let parent_table_def = match self.catalog.get_table(parent_table) {
            Some(t) => t,
            None => return Ok(()),
        };

        let referencing = self.catalog.get_tables_referencing(parent_table);
        for (child_table_def, fk) in referencing {
            let parent_col_idx = match parent_table_def.column_index(&fk.foreign_column) {
                Some(i) => i,
                None => continue,
            };
            let old_val = match old_parent_row.values.get(parent_col_idx) {
                Some(v) => v,
                None => continue,
            };
            let new_val = match new_parent_row.values.get(parent_col_idx) {
                Some(v) => v,
                None => continue,
            };

            if old_val == new_val || old_val.is_null() {
                continue;
            }

            let child_col_idx = match child_table_def.column_index(&fk.column) {
                Some(i) => i,
                None => continue,
            };

            let child_map_name = format!("tbl_{}", child_table_def.name.to_lowercase());
            let child_entries = tx.scan_visible(&child_map_name)?;
            let mut matching_child_rows = Vec::new();
            for (c_key, c_val_bytes) in child_entries {
                let mut c_row = Row::from_bytes(&c_val_bytes)?;
                child_table_def.align_row(&mut c_row);
                if c_row.values.get(child_col_idx) == Some(old_val) {
                    matching_child_rows.push((c_key, c_row));
                }
            }

            if matching_child_rows.is_empty() {
                continue;
            }

            match fk.on_update {
                ForeignKeyAction::Restrict | ForeignKeyAction::NoAction => {
                    return Err(H2Error::Execution(format!(
                        "Foreign key constraint violation: cannot update referenced key in table '{}' because record is referenced by table '{}' (foreign key on column '{}')",
                        parent_table, child_table_def.name, fk.column
                    )));
                }
                ForeignKeyAction::Cascade => {
                    let child_indexes = self.catalog.get_table_indexes(&child_table_def.name);
                    for (c_key, mut c_row) in matching_child_rows {
                        let c_row_id = if c_key.len() == 8 {
                            u64::from_le_bytes(c_key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };
                        c_row.values[child_col_idx] = new_val.clone();

                        // インデックス更新
                        for idx in &child_indexes {
                            if let Some(ci) = child_table_def.column_index(&idx.columns[0]) {
                                if ci == child_col_idx {
                                    let idx_map = format!("idx_{}_{}", child_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                    let old_idx_key = encode_index_key(old_val, c_row_id);
                                    tx.remove(&idx_map, &old_idx_key)?;
                                    let new_idx_key = encode_index_key(new_val, c_row_id);
                                    tx.put(&idx_map, new_idx_key, vec![])?;
                                }
                            }
                        }

                        tx.put(&child_map_name, c_key, c_row.to_bytes()?)?;
                    }
                }
                ForeignKeyAction::SetNull => {
                    let child_indexes = self.catalog.get_table_indexes(&child_table_def.name);
                    for (c_key, mut c_row) in matching_child_rows {
                        let c_row_id = if c_key.len() == 8 {
                            u64::from_le_bytes(c_key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };
                        c_row.values[child_col_idx] = Value::Null;

                        // インデックス更新
                        for idx in &child_indexes {
                            if let Some(ci) = child_table_def.column_index(&idx.columns[0]) {
                                if ci == child_col_idx {
                                    let idx_map = format!("idx_{}_{}", child_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                    let old_idx_key = encode_index_key(old_val, c_row_id);
                                    tx.remove(&idx_map, &old_idx_key)?;
                                    let new_idx_key = encode_index_key(&Value::Null, c_row_id);
                                    tx.put(&idx_map, new_idx_key, vec![])?;
                                }
                            }
                        }

                        tx.put(&child_map_name, c_key, c_row.to_bytes()?)?;
                    }
                }
            }
        }

        Ok(())
    }

}
