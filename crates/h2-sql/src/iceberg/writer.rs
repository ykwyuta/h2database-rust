use std::collections::HashMap;
use std::fs::{self, File};
use std::path::Path;
use chrono::Utc;
use parquet::arrow::ArrowWriter;
use uuid::Uuid;

use h2_types::{H2Error, H2Result, Value};
use crate::catalog::TableDef;
use crate::row::Row;
use super::metadata::{
    get_latest_metadata, init_iceberg_table, read_manifest_file, read_manifest_list,
    write_manifest_file, write_manifest_list, write_table_metadata, write_version_hint,
    DataFile, ManifestEntry, ManifestListEntry, Snapshot, SnapshotLogEntry,
};

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

    // 1. Arrow RecordBatch 構築
    let arrow_schema = crate::vectorized::create_arrow_schema(&table_def.columns);
    let record_batch = crate::vectorized::rows_to_record_batch(&arrow_schema, rows)?;

    // 2. Parquet ファイル書き出し
    let file_uuid = Uuid::new_v4().to_string();
    let parquet_file_name = format!("00000-{}.parquet", file_uuid);
    let parquet_path = data_dir.join(&parquet_file_name);

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

    // 3. 列統計（Min/Max/Null Count）の算出
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
            lower_bounds.insert(field_id.clone(), format!("{}", m));
        }
        if let Some(m) = max_val {
            upper_bounds.insert(field_id.clone(), format!("{}", m));
        }
    }

    let new_data_file = DataFile {
        file_path: format!("data/{}", parquet_file_name),
        file_format: "PARQUET".to_string(),
        record_count: num_rows,
        file_size_in_bytes,
        column_sizes: HashMap::new(),
        value_counts,
        null_value_counts,
        lower_bounds,
        upper_bounds,
    };

    // 4. 最新メタデータの取得
    let (latest_ver, mut metadata) = get_latest_metadata(location)?;

    // 5. 既存スナップショットの有効データファイルを引き継ぎ
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
    manifest_entries.push(ManifestEntry {
        status: 1,
        snapshot_id: new_snapshot_id,
        data_file: new_data_file,
    });

    // 6. マニフェストファイル書き出し
    let manifest_uuid = Uuid::new_v4().to_string();
    let manifest_file_name = format!("m-{}.json", manifest_uuid);
    let manifest_rel_path = write_manifest_file(location, &manifest_file_name, &manifest_entries)?;

    // 7. マニフェストリスト書き出し
    let manifest_list_file_name = format!("snap-{}.json", new_snapshot_id);
    let manifest_list_entries = vec![ManifestListEntry {
        manifest_path: manifest_rel_path,
        manifest_length: 1024,
        partition_spec_id: 0,
        added_snapshot_id: new_snapshot_id,
        added_data_files_count: 1,
        existing_data_files_count: (manifest_entries.len() - 1) as u32,
        deleted_data_files_count: 0,
        partitions: vec![],
    }];
    let manifest_list_rel_path = write_manifest_list(location, &manifest_list_file_name, &manifest_list_entries)?;

    // 8. Snapshot 登録
    let mut summary = HashMap::new();
    summary.insert("operation".to_string(), "append".to_string());
    summary.insert("added-data-files".to_string(), "1".to_string());
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

    // 9. メタデータファイル更新と version-hint.text アトミック更新
    let next_ver = latest_ver + 1;
    write_table_metadata(location, next_ver, &metadata)?;
    write_version_hint(location, next_ver)?;

    Ok(num_rows)
}
