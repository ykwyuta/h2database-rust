pub mod metadata;
pub mod reader;
pub mod writer;

pub use metadata::{
    get_latest_metadata, init_iceberg_table, read_manifest_file, read_manifest_list,
    DataFile, ManifestEntry, ManifestListEntry, PartitionField, PartitionSpec, Snapshot, TableMetadata,
};
pub use reader::read_iceberg_table_rows;
pub use writer::{compact_iceberg_table, write_iceberg_position_deletes, write_iceberg_table_rows, CompactionResult};

use h2_types::{H2Result, Value};
use crate::row::Row;

pub fn get_iceberg_snapshots(location: &str) -> H2Result<Vec<Row>> {
    let (_ver, meta) = get_latest_metadata(location)?;
    let mut rows = Vec::new();

    for s in meta.snapshots {
        let op = s.summary.get("operation").cloned().unwrap_or_else(|| "unknown".to_string());
        let added_records = s.summary.get("added-records")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let total_records = s.summary.get("total-records")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);

        rows.push(Row::new(vec![
            Value::BigInt(s.snapshot_id),
            match s.parent_snapshot_id {
                Some(p) => Value::BigInt(p),
                None => Value::Null,
            },
            Value::BigInt(s.timestamp_ms),
            Value::String(op),
            Value::BigInt(added_records),
            Value::BigInt(total_records),
            Value::String(s.manifest_list),
        ]));
    }

    Ok(rows)
}

pub fn get_iceberg_files(location: &str) -> H2Result<Vec<Row>> {
    let (_ver, meta) = get_latest_metadata(location)?;
    let mut rows = Vec::new();

    if let Some(snapshot_id) = meta.current_snapshot_id {
        if let Some(snapshot) = meta.snapshots.iter().find(|s| s.snapshot_id == snapshot_id) {
            let manifest_list = metadata::read_manifest_list(location, &snapshot.manifest_list)?;
            for m_entry in manifest_list {
                let manifest_entries = metadata::read_manifest_file(location, &m_entry.manifest_path)?;
                for entry in manifest_entries {
                    if entry.status != 2 { // Not DELETED
                        let df = entry.data_file;
                        let lower_str = serde_json::to_string(&df.lower_bounds).unwrap_or_default();
                        let upper_str = serde_json::to_string(&df.upper_bounds).unwrap_or_default();

                        rows.push(Row::new(vec![
                            Value::BigInt(entry.snapshot_id),
                            Value::String(df.file_path),
                            Value::String(df.file_format),
                            Value::BigInt(df.record_count as i64),
                            Value::BigInt(df.file_size_in_bytes as i64),
                            Value::String(lower_str),
                            Value::String(upper_str),
                        ]));
                    }
                }
            }
        }
    }

    Ok(rows)
}
