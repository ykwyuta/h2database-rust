use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;
use chrono::Utc;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use sqlparser::ast::Expr;
use uuid::Uuid;

use arrow::array::{as_primitive_array, as_string_array, Int64Array, StringArray};
use arrow::datatypes::{DataType as ArrowDataType, Field as ArrowField, Int64Type, Schema as ArrowSchema};

use h2_types::{H2Error, H2Result, Value};
use crate::catalog::TableDef;
use crate::expression::{evaluate_expr_context, RowContext};
use crate::row::Row;
use super::metadata::{
    apply_partition_transform, get_latest_metadata, init_iceberg_table, read_manifest_file,
    read_manifest_list, resolve_path, write_manifest_file, write_manifest_list,
    write_table_metadata, write_version_hint, DataFile, ManifestEntry, ManifestListEntry,
    Snapshot, SnapshotLogEntry,
};

fn format_bound_value(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        _ => val.to_string(),
    }
}

fn write_parquet_data_file(
    data_dir: &Path,
    rel_dir: &str,
    part_map: HashMap<String, String>,
    table_def: &TableDef,
    rows: &[Row],
    arrow_schema: &arrow::datatypes::SchemaRef,
) -> H2Result<DataFile> {
    let target_dir = if rel_dir.is_empty() {
        data_dir.to_path_buf()
    } else {
        data_dir.join(rel_dir)
    };
    fs::create_dir_all(&target_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to create data dir {:?}: {}", target_dir, e)))?;

    let file_uuid = Uuid::new_v4().to_string();
    let parquet_file_name = format!("00000-{}.parquet", file_uuid);
    let parquet_path = target_dir.join(&parquet_file_name);

    let record_batch = crate::vectorized::rows_to_record_batch(arrow_schema, rows)?;
    let file = File::create(&parquet_path)
        .map_err(|e| H2Error::Execution(format!("Failed to create Parquet file {:?}: {}", parquet_path, e)))?;
    let mut writer = ArrowWriter::try_new(file, arrow_schema.clone(), None)
        .map_err(|e| H2Error::Execution(format!("Failed to create ArrowWriter: {}", e)))?;
    writer.write(&record_batch)
        .map_err(|e| H2Error::Execution(format!("Failed to write RecordBatch to Parquet: {}", e)))?;
    writer.close()
        .map_err(|e| H2Error::Execution(format!("Failed to close ArrowWriter: {}", e)))?;

    let file_size_in_bytes = fs::metadata(&parquet_path)
        .map_err(|e| H2Error::Execution(format!("Failed to read Parquet metadata: {}", e)))?
        .len();

    let num_rows = rows.len() as u64;
    let mut null_value_counts = HashMap::new();
    let mut value_counts = HashMap::new();
    let mut lower_bounds = HashMap::new();
    let mut upper_bounds = HashMap::new();

    for (col_idx, _col_def) in table_def.columns.iter().enumerate() {
        let field_id = (col_idx + 1).to_string();
        let mut null_count = 0u64;
        let mut min_val: Option<Value> = None;
        let mut max_val: Option<Value> = None;

        for row in rows {
            match row.get(col_idx) {
                Some(Value::Null) | None => null_count += 1,
                Some(val) => {
                    match &min_val {
                        None => min_val = Some(val.clone()),
                        Some(m) if val < m => min_val = Some(val.clone()),
                        _ => {}
                    }
                    match &max_val {
                        None => max_val = Some(val.clone()),
                        Some(m) if val > m => max_val = Some(val.clone()),
                        _ => {}
                    }
                }
            }
        }

        null_value_counts.insert(field_id.clone(), null_count);
        value_counts.insert(field_id.clone(), num_rows);

        if let Some(m) = min_val {
            lower_bounds.insert(field_id.clone(), format_bound_value(&m));
        }
        if let Some(m) = max_val {
            upper_bounds.insert(field_id.clone(), format_bound_value(&m));
        }
    }

    let rel_file_path = if rel_dir.is_empty() {
        format!("data/{}", parquet_file_name)
    } else {
        format!("data/{}/{}", rel_dir.replace('\\', "/"), parquet_file_name)
    };

    Ok(DataFile {
        content: 0,
        file_path: rel_file_path,
        file_format: "PARQUET".to_string(),
        partition: part_map,
        record_count: num_rows,
        file_size_in_bytes,
        column_sizes: HashMap::new(),
        value_counts,
        null_value_counts,
        lower_bounds,
        upper_bounds,
        equality_ids: None,
    })
}

pub fn write_iceberg_table_rows(table_def: &TableDef, rows: &[Row]) -> H2Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }

    let location = table_def.iceberg_location.as_deref().ok_or_else(|| {
        H2Error::Execution(format!("Iceberg table '{}' has no location defined", table_def.name))
    })?;

    // テーブルが未初期化の場合は初期化
    init_iceberg_table(location, table_def)?;

    let base_path = Path::new(location);
    let data_dir = base_path.join("data");
    fs::create_dir_all(&data_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to create data directory {:?}: {}", data_dir, e)))?;

    let arrow_schema = crate::vectorized::create_arrow_schema(&table_def.columns);

    // パーティショニングの有無に応じてグループ化
    let is_partitioned = !table_def.iceberg_partition_fields.is_empty();
    let grouped_rows: Vec<(HashMap<String, String>, String, Vec<Row>)> = if is_partitioned {
        let mut map: HashMap<Vec<(String, String)>, Vec<Row>> = HashMap::new();
        for row in rows {
            let mut key = Vec::new();
            for pf in &table_def.iceberg_partition_fields {
                let col_idx = (pf.source_id - 1) as usize;
                let val = row.get(col_idx).unwrap_or(&Value::Null);
                let transformed = apply_partition_transform(&pf.transform, val);
                key.push((pf.name.clone(), transformed));
            }
            map.entry(key).or_default().push(row.clone());
        }
        map.into_iter().map(|(k_vec, r_vec)| {
            let rel_dir = k_vec.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join("/");
            let part_map: HashMap<String, String> = k_vec.into_iter().collect();
            (part_map, rel_dir, r_vec)
        }).collect()
    } else {
        vec![(HashMap::new(), String::new(), rows.to_vec())]
    };

    let mut new_data_files = Vec::new();
    for (part_map, rel_dir, g_rows) in grouped_rows {
        let df = write_parquet_data_file(&data_dir, &rel_dir, part_map, table_def, &g_rows, &arrow_schema)?;
        new_data_files.push(df);
    }

    let num_rows = rows.len() as u64;

    // 最新メタデータの取得
    let (latest_ver, mut metadata) = get_latest_metadata(location)?;

    // 既存スナップショットの有効データファイルを引き継ぎ
    let mut manifest_entries = Vec::new();
    let mut total_records = num_rows;

    let now_ms = Utc::now().timestamp_millis();
    let new_snapshot_id = now_ms * 1000 + (now_ms % 997);
    let new_seq = metadata.last_sequence_number + 1;

    if let Some(prev_snapshot_id) = metadata.current_snapshot_id {
        if let Some(prev_snapshot) = metadata.snapshots.iter().find(|s| s.snapshot_id == prev_snapshot_id) {
            if let Ok(prev_manifest_list) = read_manifest_list(location, &prev_snapshot.manifest_list) {
                for m_entry in prev_manifest_list {
                    if let Ok(entries) = read_manifest_file(location, &m_entry.manifest_path) {
                        for mut entry in entries {
                            if entry.status != 2 { // 2 = DELETED
                                total_records += entry.data_file.record_count;
                                entry.status = 0; // 0 = EXISTING
                                manifest_entries.push(entry);
                            }
                        }
                    }
                }
            }
        }
    }

    // 新規データファイルを追加 (status = 1: ADDED)
    let added_count = new_data_files.len() as u32;
    for df in new_data_files {
        manifest_entries.push(ManifestEntry {
            status: 1,
            snapshot_id: new_snapshot_id,
            data_file: df,
        });
    }

    // マニフェストファイル書き出し
    let manifest_uuid = Uuid::new_v4().to_string();
    let manifest_file_name = format!("m-{}.json", manifest_uuid);
    let manifest_rel_path = write_manifest_file(location, &manifest_file_name, &manifest_entries)?;

    // マニフェストリスト書き出し
    let manifest_list_file_name = format!("snap-{}.json", new_snapshot_id);
    let manifest_list_entries = vec![ManifestListEntry {
        manifest_path: manifest_rel_path,
        manifest_length: 1024,
        partition_spec_id: 0,
        added_snapshot_id: new_snapshot_id,
        added_data_files_count: added_count,
        existing_data_files_count: (manifest_entries.len() - added_count as usize) as u32,
        deleted_data_files_count: 0,
        partitions: vec![],
    }];
    let manifest_list_rel_path = write_manifest_list(location, &manifest_list_file_name, &manifest_list_entries)?;

    // Snapshot 登録
    let mut summary = HashMap::new();
    summary.insert("operation".to_string(), "append".to_string());
    summary.insert("added-data-files".to_string(), added_count.to_string());
    summary.insert("added-records".to_string(), num_rows.to_string());
    summary.insert("total-records".to_string(), total_records.to_string());

    let new_snapshot = Snapshot {
        snapshot_id: new_snapshot_id,
        parent_snapshot_id: metadata.current_snapshot_id,
        sequence_number: new_seq,
        timestamp_ms: now_ms,
        manifest_list: manifest_list_rel_path,
        summary,
    };

    metadata.last_sequence_number = new_seq;
    metadata.last_updated_ms = now_ms;
    metadata.current_snapshot_id = Some(new_snapshot_id);
    metadata.snapshots.push(new_snapshot);
    metadata.snapshot_log.push(SnapshotLogEntry {
        timestamp_ms: now_ms,
        snapshot_id: new_snapshot_id,
    });

    let next_ver = latest_ver + 1;
    write_table_metadata(location, next_ver, &metadata)?;
    write_version_hint(location, next_ver)?;

    Ok(num_rows)
}

/// Iceberg v2 Position Deletes による行削除
pub fn write_iceberg_position_deletes(
    table_def: &TableDef,
    selection: Option<&Expr>,
) -> H2Result<u64> {
    let location = table_def.iceberg_location.as_deref().ok_or_else(|| {
        H2Error::Execution(format!("Iceberg table '{}' has no location defined", table_def.name))
    })?;

    if !Path::new(location).join("metadata").exists() {
        return Ok(0);
    }

    let (latest_ver, mut metadata) = get_latest_metadata(location)?;
    let snapshot_id = match metadata.current_snapshot_id {
        Some(id) => id,
        None => return Ok(0),
    };

    let snapshot = match metadata.snapshots.iter().find(|s| s.snapshot_id == snapshot_id) {
        Some(s) => s,
        None => return Ok(0),
    };

    let manifest_list = read_manifest_list(location, &snapshot.manifest_list)?;
    let mut active_manifest_entries: Vec<ManifestEntry> = Vec::new();
    let mut data_files: Vec<DataFile> = Vec::new();
    let mut delete_files: Vec<DataFile> = Vec::new();

    for m_entry in manifest_list {
        let manifest_entries = read_manifest_file(location, &m_entry.manifest_path)?;
        for mut entry in manifest_entries {
            if entry.status != 2 {
                if entry.data_file.content == 1 || entry.data_file.content == 2 {
                    delete_files.push(entry.data_file.clone());
                } else {
                    data_files.push(entry.data_file.clone());
                }
                entry.status = 0; // Existing
                active_manifest_entries.push(entry);
            }
        }
    }

    // 既存の Position Deletes をロード
    let mut deleted_positions: HashMap<String, HashSet<i64>> = HashMap::new();
    for df in &delete_files {
        if df.content == 1 {
            let p = resolve_path(location, &df.file_path);
            if p.exists() {
                if let Ok(file) = File::open(&p) {
                    if let Ok(builder) = ParquetRecordBatchReaderBuilder::try_new(file) {
                        if let Ok(mut reader) = builder.build() {
                            while let Some(Ok(batch)) = reader.next() {
                                if batch.num_columns() >= 2 {
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

    // データファイルを走査し、削除条件にマッチする (file_path, pos) を抽出
    let mut to_delete: Vec<(String, i64)> = Vec::new();
    let ctx = RowContext::from_table_def(table_def, None);

    for df in &data_files {
        let p = resolve_path(location, &df.file_path);
        if !p.exists() {
            continue;
        }

        let file = File::open(&p)
            .map_err(|e| H2Error::Execution(format!("Failed to open Parquet file {:?}: {}", p, e)))?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| H2Error::Execution(format!("Failed to create Parquet reader: {}", e)))?;
        let mut reader = builder.build()
            .map_err(|e| H2Error::Execution(format!("Failed to build reader: {}", e)))?;

        let mut current_pos: i64 = 0;
        let norm_df_path = df.file_path.replace('\\', "/");

        while let Some(batch_res) = reader.next() {
            let batch = batch_res
                .map_err(|e| H2Error::Execution(format!("Failed to read batch: {}", e)))?;
            for row_idx in 0..batch.num_rows() {
                let pos = current_pos;
                current_pos += 1;

                if deleted_positions.get(&norm_df_path).map_or(false, |s| s.contains(&pos)) {
                    continue; // 既に削除済み
                }

                let mut row = crate::vectorized::record_batch_to_row(&batch, row_idx)?;
                table_def.align_row(&mut row);

                let is_match = if let Some(sel) = selection {
                    match evaluate_expr_context(sel, &ctx, &row)? {
                        Value::Boolean(b) => b,
                        _ => false,
                    }
                } else {
                    true
                };

                if is_match {
                    to_delete.push((norm_df_path.clone(), pos));
                }
            }
        }
    }

    if to_delete.is_empty() {
        return Ok(0);
    }

    // Position Delete Parquet ファイルの生成
    let deletes_dir = Path::new(location).join("data").join("deletes");
    fs::create_dir_all(&deletes_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to create deletes dir: {}", e)))?;

    let del_uuid = Uuid::new_v4().to_string();
    let del_file_name = format!("delete-pos-{}.parquet", del_uuid);
    let del_file_path = deletes_dir.join(&del_file_name);

    let delete_arrow_schema = Arc::new(ArrowSchema::new(vec![
        ArrowField::new("file_path", ArrowDataType::Utf8, false),
        ArrowField::new("pos", ArrowDataType::Int64, false),
    ]));

    let paths_array = Arc::new(StringArray::from_iter_values(to_delete.iter().map(|(p, _)| p.as_str())));
    let pos_array = Arc::new(Int64Array::from_iter_values(to_delete.iter().map(|(_, pos)| *pos)));

    let record_batch = arrow::record_batch::RecordBatch::try_new(
        delete_arrow_schema.clone(),
        vec![paths_array, pos_array],
    ).map_err(|e| H2Error::Execution(format!("Failed to create delete RecordBatch: {}", e)))?;

    let file = File::create(&del_file_path)
        .map_err(|e| H2Error::Execution(format!("Failed to create delete Parquet file: {}", e)))?;
    let mut writer = ArrowWriter::try_new(file, delete_arrow_schema, None)
        .map_err(|e| H2Error::Execution(format!("Failed to create ArrowWriter for deletes: {}", e)))?;
    writer.write(&record_batch)
        .map_err(|e| H2Error::Execution(format!("Failed to write deletes batch: {}", e)))?;
    writer.close()
        .map_err(|e| H2Error::Execution(format!("Failed to close deletes writer: {}", e)))?;

    let del_file_size = fs::metadata(&del_file_path)
        .map_err(|e| H2Error::Execution(format!("Failed to read metadata: {}", e)))?
        .len();

    let num_deleted = to_delete.len() as u64;

    let delete_data_file = DataFile {
        content: 1, // POSITION_DELETES
        file_path: format!("data/deletes/{}", del_file_name),
        file_format: "PARQUET".to_string(),
        partition: HashMap::new(),
        record_count: num_deleted,
        file_size_in_bytes: del_file_size,
        column_sizes: HashMap::new(),
        value_counts: HashMap::new(),
        null_value_counts: HashMap::new(),
        lower_bounds: HashMap::new(),
        upper_bounds: HashMap::new(),
        equality_ids: None,
    };

    let now_ms = Utc::now().timestamp_millis();
    let new_snapshot_id = now_ms * 1000 + (now_ms % 997);
    let new_seq = metadata.last_sequence_number + 1;

    active_manifest_entries.push(ManifestEntry {
        status: 1, // ADDED
        snapshot_id: new_snapshot_id,
        data_file: delete_data_file,
    });

    // マニフェストファイル書き出し
    let manifest_uuid = Uuid::new_v4().to_string();
    let manifest_file_name = format!("m-{}.json", manifest_uuid);
    let manifest_rel_path = write_manifest_file(location, &manifest_file_name, &active_manifest_entries)?;

    // マニフェストリスト書き出し
    let manifest_list_file_name = format!("snap-{}.json", new_snapshot_id);
    let manifest_list_entries = vec![ManifestListEntry {
        manifest_path: manifest_rel_path,
        manifest_length: 1024,
        partition_spec_id: 0,
        added_snapshot_id: new_snapshot_id,
        added_data_files_count: 0,
        existing_data_files_count: (active_manifest_entries.len() - 1) as u32,
        deleted_data_files_count: 0,
        partitions: vec![],
    }];
    let manifest_list_rel_path = write_manifest_list(location, &manifest_list_file_name, &manifest_list_entries)?;

    // Snapshot コミット
    let mut summary = HashMap::new();
    summary.insert("operation".to_string(), "delete".to_string());
    summary.insert("added-delete-files".to_string(), "1".to_string());
    summary.insert("added-position-delete-records".to_string(), num_deleted.to_string());
    summary.insert("deleted-records".to_string(), num_deleted.to_string());

    let new_snapshot = Snapshot {
        snapshot_id: new_snapshot_id,
        parent_snapshot_id: metadata.current_snapshot_id,
        sequence_number: new_seq,
        timestamp_ms: now_ms,
        manifest_list: manifest_list_rel_path,
        summary,
    };

    metadata.last_sequence_number = new_seq;
    metadata.last_updated_ms = now_ms;
    metadata.current_snapshot_id = Some(new_snapshot_id);
    metadata.snapshots.push(new_snapshot);
    metadata.snapshot_log.push(SnapshotLogEntry {
        timestamp_ms: now_ms,
        snapshot_id: new_snapshot_id,
    });

    let next_ver = latest_ver + 1;
    write_table_metadata(location, next_ver, &metadata)?;
    write_version_hint(location, next_ver)?;

    Ok(num_deleted)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionResult {
    pub table_name: String,
    pub compacted: bool,
    pub files_before: u64,
    pub files_after: u64,
    pub total_records: u64,
    pub delete_files_removed: u64,
}

/// Iceberg テーブルのコンパクション（多数の小ファイルおよび差分削除ファイルを統合）
pub fn compact_iceberg_table(table_def: &TableDef) -> H2Result<CompactionResult> {
    let location = table_def.iceberg_location.as_deref().ok_or_else(|| {
        H2Error::Execution(format!("Iceberg table '{}' has no location defined", table_def.name))
    })?;

    if !Path::new(location).join("metadata").exists() {
        return Ok(CompactionResult {
            table_name: table_def.name.clone(),
            compacted: false,
            files_before: 0,
            files_after: 0,
            total_records: 0,
            delete_files_removed: 0,
        });
    }

    let (latest_ver, mut metadata) = get_latest_metadata(location)?;
    let snapshot_id = match metadata.current_snapshot_id {
        Some(id) => id,
        None => {
            return Ok(CompactionResult {
                table_name: table_def.name.clone(),
                compacted: false,
                files_before: 0,
                files_after: 0,
                total_records: 0,
                delete_files_removed: 0,
            });
        }
    };

    let snapshot = match metadata.snapshots.iter().find(|s| s.snapshot_id == snapshot_id) {
        Some(s) => s,
        None => {
            return Ok(CompactionResult {
                table_name: table_def.name.clone(),
                compacted: false,
                files_before: 0,
                files_after: 0,
                total_records: 0,
                delete_files_removed: 0,
            });
        }
    };

    let manifest_list = read_manifest_list(location, &snapshot.manifest_list)?;
    let mut old_data_files: Vec<DataFile> = Vec::new();
    let mut old_delete_files: Vec<DataFile> = Vec::new();

    for m_entry in manifest_list {
        let manifest_entries = read_manifest_file(location, &m_entry.manifest_path)?;
        for entry in manifest_entries {
            if entry.status != 2 {
                if entry.data_file.content == 1 || entry.data_file.content == 2 {
                    old_delete_files.push(entry.data_file);
                } else {
                    old_data_files.push(entry.data_file);
                }
            }
        }
    }

    let files_before = old_data_files.len() as u64;
    let delete_files_count = old_delete_files.len() as u64;

    // コンパクションの要否判定:
    // 削除ファイルが存在する、またはデータファイルが2つ以上存在する場合にコンパクションを実行
    if files_before <= 1 && delete_files_count == 0 {
        let total_rec = old_data_files.iter().map(|d| d.record_count).sum();
        return Ok(CompactionResult {
            table_name: table_def.name.clone(),
            compacted: false,
            files_before,
            files_after: files_before,
            total_records: total_rec,
            delete_files_removed: 0,
        });
    }

    // 1. 現スナップショットの有効レコードを MoR で読み出し（削除位置・削除キー適用済み）
    let surviving_rows = super::reader::read_iceberg_table_rows(table_def, None, Some(snapshot_id))?;
    let total_records = surviving_rows.len() as u64;

    // 2. 最適化された Parquet データファイル（単一またはパーティション別）を書き出し
    let base_path = Path::new(location);
    let data_dir = base_path.join("data");
    fs::create_dir_all(&data_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to create data dir {:?}: {}", data_dir, e)))?;

    let arrow_schema = crate::vectorized::create_arrow_schema(&table_def.columns);
    let is_partitioned = !table_def.iceberg_partition_fields.is_empty();

    let grouped_rows: Vec<(HashMap<String, String>, String, Vec<Row>)> = if is_partitioned {
        let mut map: HashMap<Vec<(String, String)>, Vec<Row>> = HashMap::new();
        for row in &surviving_rows {
            let mut key = Vec::new();
            for pf in &table_def.iceberg_partition_fields {
                let col_idx = (pf.source_id - 1) as usize;
                let val = row.get(col_idx).unwrap_or(&Value::Null);
                let transformed = apply_partition_transform(&pf.transform, val);
                key.push((pf.name.clone(), transformed));
            }
            map.entry(key).or_default().push(row.clone());
        }
        map.into_iter().map(|(k_vec, r_vec)| {
            let rel_dir = k_vec.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join("/");
            let part_map: HashMap<String, String> = k_vec.into_iter().collect();
            (part_map, rel_dir, r_vec)
        }).collect()
    } else {
        vec![(HashMap::new(), String::new(), surviving_rows.clone())]
    };

    let mut new_data_files = Vec::new();
    for (part_map, rel_dir, g_rows) in grouped_rows {
        if !g_rows.is_empty() {
            let df = write_parquet_data_file(&data_dir, &rel_dir, part_map, table_def, &g_rows, &arrow_schema)?;
            new_data_files.push(df);
        }
    }

    let files_after = new_data_files.len() as u64;

    // 3. マニフェストエントリ構築
    let now_ms = Utc::now().timestamp_millis();
    let new_snapshot_id = now_ms * 1000 + (now_ms % 997);
    let new_seq = metadata.last_sequence_number + 1;

    let mut new_manifest_entries = Vec::new();
    for o_df in old_data_files {
        new_manifest_entries.push(ManifestEntry {
            status: 2, // DELETED
            snapshot_id: new_snapshot_id,
            data_file: o_df,
        });
    }
    for o_del in old_delete_files {
        new_manifest_entries.push(ManifestEntry {
            status: 2, // DELETED
            snapshot_id: new_snapshot_id,
            data_file: o_del,
        });
    }
    for n_df in new_data_files {
        new_manifest_entries.push(ManifestEntry {
            status: 1, // ADDED
            snapshot_id: new_snapshot_id,
            data_file: n_df,
        });
    }

    // 4. マニフェストファイル書き出し
    let manifest_uuid = Uuid::new_v4().to_string();
    let manifest_file_name = format!("m-compact-{}.json", manifest_uuid);
    let manifest_rel_path = write_manifest_file(location, &manifest_file_name, &new_manifest_entries)?;

    // 5. マニフェストリスト書き出し
    let manifest_list_file_name = format!("snap-compact-{}.json", new_snapshot_id);
    let manifest_list_entries = vec![ManifestListEntry {
        manifest_path: manifest_rel_path,
        manifest_length: 1024,
        partition_spec_id: 0,
        added_snapshot_id: new_snapshot_id,
        added_data_files_count: files_after as u32,
        existing_data_files_count: 0,
        deleted_data_files_count: (files_before + delete_files_count) as u32,
        partitions: vec![],
    }];
    let manifest_list_rel_path = write_manifest_list(location, &manifest_list_file_name, &manifest_list_entries)?;

    // 6. Snapshot コミット (operation: "replace")
    let mut summary = HashMap::new();
    summary.insert("operation".to_string(), "replace".to_string());
    summary.insert("deleted-data-files".to_string(), files_before.to_string());
    summary.insert("removed-delete-files".to_string(), delete_files_count.to_string());
    summary.insert("added-data-files".to_string(), files_after.to_string());
    summary.insert("total-records".to_string(), total_records.to_string());

    let new_snapshot = Snapshot {
        snapshot_id: new_snapshot_id,
        parent_snapshot_id: metadata.current_snapshot_id,
        sequence_number: new_seq,
        timestamp_ms: now_ms,
        manifest_list: manifest_list_rel_path,
        summary,
    };

    metadata.last_sequence_number = new_seq;
    metadata.last_updated_ms = now_ms;
    metadata.current_snapshot_id = Some(new_snapshot_id);
    metadata.snapshots.push(new_snapshot);
    metadata.snapshot_log.push(SnapshotLogEntry {
        timestamp_ms: now_ms,
        snapshot_id: new_snapshot_id,
    });

    let next_ver = latest_ver + 1;
    write_table_metadata(location, next_ver, &metadata)?;
    write_version_hint(location, next_ver)?;

    Ok(CompactionResult {
        table_name: table_def.name.clone(),
        compacted: true,
        files_before,
        files_after,
        total_records,
        delete_files_removed: delete_files_count,
    })
}

