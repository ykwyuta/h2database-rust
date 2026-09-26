use std::fs::File;
use std::path::Path;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use sqlparser::ast::{BinaryOperator, Expr, Value as SqlValue};

use h2_types::{H2Error, H2Result};
use crate::catalog::TableDef;
use crate::row::Row;
use super::metadata::{get_latest_metadata, read_manifest_file, read_manifest_list, resolve_path, DataFile};

pub fn read_iceberg_table_rows(
    table_def: &TableDef,
    selection: Option<&Expr>,
    target_snapshot_id: Option<i64>,
) -> H2Result<Vec<Row>> {
    let location = table_def.iceberg_location.as_deref().ok_or_else(|| {
        H2Error::Execution(format!("Iceberg table '{}' has no location defined", table_def.name))
    })?;

    // メタデータファイルが存在しない場合は空テーブルとして扱う
    if !Path::new(location).join("metadata").exists() {
        return Ok(Vec::new());
    }

    let (_ver, metadata) = match get_latest_metadata(location) {
        Ok(m) => m,
        Err(_) => return Ok(Vec::new()),
    };

    let snapshot_id = target_snapshot_id.or(metadata.current_snapshot_id);
    let snapshot_id = match snapshot_id {
        Some(id) => id,
        None => return Ok(Vec::new()), // スナップショットがない場合は空
    };

    let snapshot = match metadata.snapshots.iter().find(|s| s.snapshot_id == snapshot_id) {
        Some(s) => s,
        None => return Err(H2Error::Execution(format!("Snapshot {} not found in Iceberg table '{}'", snapshot_id, table_def.name))),
    };

    let manifest_list = read_manifest_list(location, &snapshot.manifest_list)?;
    let mut data_files: Vec<DataFile> = Vec::new();
    let mut delete_files: Vec<DataFile> = Vec::new();

    for m_entry in manifest_list {
        let manifest_entries = read_manifest_file(location, &m_entry.manifest_path)?;
        for entry in manifest_entries {
            if entry.status != 2 { // 2 = DELETED
                if entry.data_file.content == 1 || entry.data_file.content == 2 {
                    delete_files.push(entry.data_file);
                } else {
                    data_files.push(entry.data_file);
                }
            }
        }
    }

    // 削除ファイル（Position Deletes）の読み込み
    let mut deleted_positions: std::collections::HashMap<String, std::collections::HashSet<i64>> = std::collections::HashMap::new();
    for df in &delete_files {
        if df.content == 1 {
            let parquet_path = resolve_path(location, &df.file_path);
            if parquet_path.exists() {
                if let Ok(file) = File::open(&parquet_path) {
                    if let Ok(builder) = ParquetRecordBatchReaderBuilder::try_new(file) {
                        if let Ok(mut reader) = builder.build() {
                            while let Some(Ok(batch)) = reader.next() {
                                if batch.num_columns() >= 2 {
                                    use arrow::array::{as_primitive_array, as_string_array};
                                    use arrow::datatypes::Int64Type;
                                    let paths = as_string_array(batch.column(0));
                                    let positions = as_primitive_array::<Int64Type>(batch.column(1));
                                    for i in 0..batch.num_rows() {
                                        let file_p = paths.value(i).replace('\\', "/");
                                        let pos = positions.value(i);
                                        deleted_positions.entry(file_p).or_default().insert(pos);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut rows = Vec::new();

    for df in data_files {
        if let Some(sel) = selection {
            // 1. パーティションプルーニング判定
            if should_prune_by_partition(&df, sel, table_def) {
                continue;
            }
            // 2. データスキッピング判定（Min/Max 枝刈り）
            if should_skip_data_file(&df, sel, table_def) {
                continue;
            }
        }

        let parquet_path = resolve_path(location, &df.file_path);
        if !parquet_path.exists() {
            continue;
        }

        let file = File::open(&parquet_path)
            .map_err(|e| H2Error::Execution(format!("Failed to open Parquet file {:?}: {}", parquet_path, e)))?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| H2Error::Execution(format!("Failed to create Parquet reader {:?}: {}", parquet_path, e)))?;
        let mut reader = builder.build()
            .map_err(|e| H2Error::Execution(format!("Failed to build Parquet record batch reader: {}", e)))?;

        let mut current_pos: i64 = 0;
        let norm_df_path = df.file_path.replace('\\', "/");

        while let Some(batch_res) = reader.next() {
            let batch = batch_res
                .map_err(|e| H2Error::Execution(format!("Parquet batch read error in {:?}: {}", parquet_path, e)))?;
            for row_idx in 0..batch.num_rows() {
                let pos = current_pos;
                current_pos += 1;

                // Merge-on-Read: 差分削除位置に含まれている行は除外
                if deleted_positions.get(&norm_df_path).map_or(false, |set| set.contains(&pos)) {
                    continue;
                }

                let mut row = crate::vectorized::record_batch_to_row(&batch, row_idx)?;
                table_def.align_row(&mut row);
                rows.push(row);
            }
        }
    }

    Ok(rows)
}

/// パーティション定義（Directory / Partition Values）に基づくプルーニング（枝刈り）
fn should_prune_by_partition(data_file: &DataFile, expr: &Expr, table_def: &TableDef) -> bool {
    if data_file.partition.is_empty() {
        return false;
    }
    match expr {
        Expr::BinaryOp { left, op, right } => {
            if *op == BinaryOperator::And {
                return should_prune_by_partition(data_file, left, table_def)
                    || should_prune_by_partition(data_file, right, table_def);
            }
            if *op == BinaryOperator::Or {
                return should_prune_by_partition(data_file, left, table_def)
                    && should_prune_by_partition(data_file, right, table_def);
            }

            let (col_name, val_str) = match (left.as_ref(), right.as_ref()) {
                (Expr::Identifier(ident), Expr::Value(val)) => (ident.value.clone(), extract_sql_value_str(val)),
                (Expr::Value(val), Expr::Identifier(ident)) => (ident.value.clone(), extract_sql_value_str(val)),
                _ => return false,
            };

            let val_str = match val_str {
                Some(v) => v,
                None => return false,
            };

            let part_val = data_file.partition.iter().find_map(|(k, v)| {
                if k.eq_ignore_ascii_case(&col_name) {
                    Some(v.as_str())
                } else {
                    None
                }
            }).or_else(|| {
                table_def.iceberg_partition_fields.iter().find_map(|f| {
                    if let Some(col_def) = table_def.columns.get((f.source_id - 1) as usize) {
                        if col_def.name.eq_ignore_ascii_case(&col_name) {
                            return data_file.partition.iter().find_map(|(pk, pv)| {
                                if pk.eq_ignore_ascii_case(&f.name) {
                                    Some(pv.as_str())
                                } else {
                                    None
                                }
                            });
                        }
                    }
                    None
                })
            });

            if let Some(pv) = part_val {
                let pv = pv.trim_matches('\'');
                let val_trimmed = val_str.trim_matches('\'');
                match op {
                    BinaryOperator::Eq => {
                        if !pv.eq_ignore_ascii_case(val_trimmed) {
                            return true; // 枝刈り
                        }
                    }
                    BinaryOperator::NotEq => {
                        if pv.eq_ignore_ascii_case(val_trimmed) {
                            return true; // 枝刈り
                        }
                    }
                    _ => {}
                }
            }
            false
        }
        Expr::Nested(inner) => should_prune_by_partition(data_file, inner, table_def),
        _ => false,
    }
}

/// 列統計（lower_bounds / upper_bounds）を用いて不要な Parquet ファイルを枝刈り（Data Skipping）
fn should_skip_data_file(data_file: &DataFile, expr: &Expr, table_def: &TableDef) -> bool {
    match expr {
        Expr::BinaryOp { left, op, right } => {
            let (col_name, val_str) = match (left.as_ref(), right.as_ref()) {
                (Expr::Identifier(ident), Expr::Value(val)) => (ident.value.clone(), extract_sql_value_str(val)),
                (Expr::Value(val), Expr::Identifier(ident)) => (ident.value.clone(), extract_sql_value_str(val)),
                _ => return false,
            };

            let val_str = match val_str {
                Some(v) => v,
                None => return false,
            };

            let col_idx = match table_def.column_index(&col_name) {
                Some(idx) => (idx + 1) as i32, // Iceberg 1-indexed field ID
                None => return false,
            };

            let col_key = col_idx.to_string();
            let lower = data_file.lower_bounds.get(&col_key);
            let upper = data_file.upper_bounds.get(&col_key);

            if let (Some(l_raw), Some(u_raw)) = (lower, upper) {
                let l_str = l_raw.trim_matches('\'');
                let u_str = u_raw.trim_matches('\'');
                let val_trimmed = val_str.trim_matches('\'');
                match op {
                    BinaryOperator::Eq => {
                        if is_less_than(val_trimmed, l_str) || is_greater_than(val_trimmed, u_str) {
                            return true; // 探索値が [lower, upper] の範囲外なのでスキップ可能
                        }
                    }
                    BinaryOperator::Gt => {
                        // col > val: upper <= val であれば絶対にマッチしない
                        if !is_greater_than(u_str, val_trimmed) {
                            return true;
                        }
                    }
                    BinaryOperator::Lt => {
                        // col < val: lower >= val であれば絶対にマッチしない
                        if !is_less_than(l_str, val_trimmed) {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
            false
        }
        Expr::Nested(inner) => should_skip_data_file(data_file, inner, table_def),
        _ => false,
    }
}

fn extract_sql_value_str(val: &SqlValue) -> Option<String> {
    match val {
        SqlValue::Number(n, _) => Some(n.clone()),
        SqlValue::SingleQuotedString(s) | SqlValue::DoubleQuotedString(s) => Some(s.clone()),
        SqlValue::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

fn is_less_than(a: &str, b: &str) -> bool {
    if let (Ok(num_a), Ok(num_b)) = (a.parse::<f64>(), b.parse::<f64>()) {
        num_a < num_b
    } else {
        a < b
    }
}

fn is_greater_than(a: &str, b: &str) -> bool {
    if let (Ok(num_a), Ok(num_b)) = (a.parse::<f64>(), b.parse::<f64>()) {
        num_a > num_b
    } else {
        a > b
    }
}
