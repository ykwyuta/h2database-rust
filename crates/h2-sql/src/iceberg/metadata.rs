use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use h2_types::{DataType as H2DataType, H2Error, H2Result};
use crate::catalog::{ColumnDef, TableDef};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IcebergField {
    pub id: i32,
    pub name: String,
    pub required: bool,
    #[serde(rename = "type")]
    pub type_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IcebergSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    #[serde(rename = "schema-id")]
    pub schema_id: i32,
    pub fields: Vec<IcebergField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionSpec {
    #[serde(rename = "spec-id")]
    pub spec_id: i32,
    pub fields: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(rename = "snapshot-id")]
    pub snapshot_id: i64,
    #[serde(rename = "parent-snapshot-id")]
    pub parent_snapshot_id: Option<i64>,
    #[serde(rename = "sequence-number")]
    pub sequence_number: i64,
    #[serde(rename = "timestamp-ms")]
    pub timestamp_ms: i64,
    #[serde(rename = "manifest-list")]
    pub manifest_list: String,
    pub summary: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotLogEntry {
    #[serde(rename = "timestamp-ms")]
    pub timestamp_ms: i64,
    #[serde(rename = "snapshot-id")]
    pub snapshot_id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableMetadata {
    #[serde(rename = "format-version")]
    pub format_version: i32,
    #[serde(rename = "table-uuid")]
    pub table_uuid: String,
    pub location: String,
    #[serde(rename = "last-sequence-number")]
    pub last_sequence_number: i64,
    #[serde(rename = "last-updated-ms")]
    pub last_updated_ms: i64,
    #[serde(rename = "last-column-id")]
    pub last_column_id: i32,
    #[serde(rename = "current-schema-id")]
    pub current_schema_id: i32,
    pub schemas: Vec<IcebergSchema>,
    #[serde(rename = "default-spec-id")]
    pub default_spec_id: i32,
    #[serde(rename = "partition-specs")]
    pub partition_specs: Vec<PartitionSpec>,
    #[serde(rename = "last-partition-id")]
    pub last_partition_id: i32,
    #[serde(rename = "current-snapshot-id")]
    pub current_snapshot_id: Option<i64>,
    pub snapshots: Vec<Snapshot>,
    #[serde(rename = "snapshot-log")]
    pub snapshot_log: Vec<SnapshotLogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestListEntry {
    pub manifest_path: String,
    pub manifest_length: u64,
    pub partition_spec_id: i32,
    pub added_snapshot_id: i64,
    pub added_data_files_count: u32,
    pub existing_data_files_count: u32,
    pub deleted_data_files_count: u32,
    #[serde(default)]
    pub partitions: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataFile {
    pub file_path: String,
    pub file_format: String,
    pub record_count: u64,
    pub file_size_in_bytes: u64,
    #[serde(default)]
    pub column_sizes: HashMap<String, u64>,
    #[serde(default)]
    pub value_counts: HashMap<String, u64>,
    #[serde(default)]
    pub null_value_counts: HashMap<String, u64>,
    #[serde(default)]
    pub lower_bounds: HashMap<String, String>,
    #[serde(default)]
    pub upper_bounds: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub status: u8, // 0: EXISTING, 1: ADDED, 2: DELETED
    pub snapshot_id: i64,
    pub data_file: DataFile,
}

pub fn h2_type_to_iceberg_type(dt: &H2DataType) -> String {
    match dt {
        H2DataType::Boolean => "boolean".to_string(),
        H2DataType::TinyInt | H2DataType::SmallInt | H2DataType::Integer => "int".to_string(),
        H2DataType::BigInt => "long".to_string(),
        H2DataType::Float => "float".to_string(),
        H2DataType::Double => "double".to_string(),
        H2DataType::Decimal(p, s) => format!("decimal({},{})", p, s),
        H2DataType::Char(_) | H2DataType::VarChar(_) => "string".to_string(),
        H2DataType::Binary(_) | H2DataType::Blob => "binary".to_string(),
        H2DataType::Date => "date".to_string(),
        H2DataType::Time => "time".to_string(),
        H2DataType::Timestamp => "timestamp".to_string(),
        H2DataType::TimestampTz => "timestamptz".to_string(),
        H2DataType::Uuid => "uuid".to_string(),
        H2DataType::Json => "string".to_string(),
        _ => "string".to_string(),
    }
}

pub fn iceberg_type_to_h2_type(t: &str) -> H2DataType {
    match t {
        "boolean" => H2DataType::Boolean,
        "int" => H2DataType::Integer,
        "long" => H2DataType::BigInt,
        "float" => H2DataType::Float,
        "double" => H2DataType::Double,
        "string" => H2DataType::VarChar(None),
        "date" => H2DataType::Date,
        "time" => H2DataType::Time,
        "timestamp" => H2DataType::Timestamp,
        "timestamptz" => H2DataType::TimestampTz,
        "binary" => H2DataType::Binary(None),
        _ => H2DataType::VarChar(None),
    }
}

pub fn create_iceberg_schema(columns: &[ColumnDef]) -> IcebergSchema {
    let mut fields = Vec::with_capacity(columns.len());
    for (idx, col) in columns.iter().enumerate() {
        fields.push(IcebergField {
            id: (idx + 1) as i32,
            name: col.name.clone(),
            required: !col.is_nullable,
            type_name: h2_type_to_iceberg_type(&col.data_type),
        });
    }
    IcebergSchema {
        schema_type: "struct".to_string(),
        schema_id: 0,
        fields,
    }
}

pub fn resolve_path(base_location: &str, relative_or_abs: &str) -> PathBuf {
    let p = Path::new(relative_or_abs);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(base_location).join(p)
    }
}

pub fn read_version_hint(location: &str) -> H2Result<Option<u64>> {
    let hint_file = Path::new(location).join("metadata").join("version-hint.text");
    if !hint_file.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&hint_file)
        .map_err(|e| H2Error::Execution(format!("Failed to read version-hint.text: {}", e)))?;
    let ver: u64 = content.trim().parse()
        .map_err(|e| H2Error::Execution(format!("Invalid version in version-hint.text: {}", e)))?;
    Ok(Some(ver))
}

pub fn write_version_hint(location: &str, version: u64) -> H2Result<()> {
    let meta_dir = Path::new(location).join("metadata");
    fs::create_dir_all(&meta_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to create metadata dir: {}", e)))?;
    let hint_file = meta_dir.join("version-hint.text");
    fs::write(&hint_file, version.to_string())
        .map_err(|e| H2Error::Execution(format!("Failed to write version-hint.text: {}", e)))?;
    Ok(())
}

pub fn read_table_metadata(location: &str, version: u64) -> H2Result<TableMetadata> {
    let meta_file = Path::new(location).join("metadata").join(format!("v{}.metadata.json", version));
    if !meta_file.exists() {
        return Err(H2Error::Execution(format!("Metadata file not found: {:?}", meta_file)));
    }
    let content = fs::read_to_string(&meta_file)
        .map_err(|e| H2Error::Execution(format!("Failed to read metadata file {:?}: {}", meta_file, e)))?;
    let metadata: TableMetadata = serde_json::from_str(&content)
        .map_err(|e| H2Error::Execution(format!("Failed to parse metadata json: {}", e)))?;
    Ok(metadata)
}

pub fn get_latest_metadata(location: &str) -> H2Result<(u64, TableMetadata)> {
    if let Some(ver) = read_version_hint(location)? {
        let meta = read_table_metadata(location, ver)?;
        return Ok((ver, meta));
    }
    // version-hint.text がない場合はディレクトリ内の最大の v*.metadata.json を走査
    let meta_dir = Path::new(location).join("metadata");
    if !meta_dir.exists() {
        return Err(H2Error::Execution(format!("Iceberg metadata directory not found: {:?}", meta_dir)));
    }
    let mut max_ver = 0u64;
    for entry in fs::read_dir(&meta_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to read metadata dir: {}", e)))?
    {
        if let Ok(entry) = entry {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.starts_with('v') && file_name.ends_with(".metadata.json") {
                let ver_str = &file_name[1..file_name.len() - ".metadata.json".len()];
                if let Ok(ver) = ver_str.parse::<u64>() {
                    if ver > max_ver {
                        max_ver = ver;
                    }
                }
            }
        }
    }
    if max_ver == 0 {
        return Err(H2Error::Execution(format!("No metadata files found in {:?}", meta_dir)));
    }
    let meta = read_table_metadata(location, max_ver)?;
    Ok((max_ver, meta))
}

pub fn write_table_metadata(location: &str, version: u64, metadata: &TableMetadata) -> H2Result<()> {
    let meta_dir = Path::new(location).join("metadata");
    fs::create_dir_all(&meta_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to create metadata dir: {}", e)))?;
    let meta_file = meta_dir.join(format!("v{}.metadata.json", version));
    let json_bytes = serde_json::to_vec_pretty(metadata)
        .map_err(|e| H2Error::Execution(format!("Failed to serialize metadata: {}", e)))?;
    fs::write(&meta_file, json_bytes)
        .map_err(|e| H2Error::Execution(format!("Failed to write metadata file {:?}: {}", meta_file, e)))?;
    Ok(())
}

pub fn read_manifest_list(location: &str, manifest_list_path: &str) -> H2Result<Vec<ManifestListEntry>> {
    let p = resolve_path(location, manifest_list_path);
    if !p.exists() {
        return Err(H2Error::Execution(format!("Manifest list not found: {:?}", p)));
    }
    let content = fs::read_to_string(&p)
        .map_err(|e| H2Error::Execution(format!("Failed to read manifest list {:?}: {}", p, e)))?;
    let entries: Vec<ManifestListEntry> = serde_json::from_str(&content)
        .map_err(|e| H2Error::Execution(format!("Failed to parse manifest list {:?}: {}", p, e)))?;
    Ok(entries)
}

pub fn write_manifest_list(
    location: &str,
    file_name: &str,
    entries: &[ManifestListEntry],
) -> H2Result<String> {
    let meta_dir = Path::new(location).join("metadata");
    fs::create_dir_all(&meta_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to create metadata dir: {}", e)))?;
    let file_path = meta_dir.join(file_name);
    let json_bytes = serde_json::to_vec_pretty(entries)
        .map_err(|e| H2Error::Execution(format!("Failed to serialize manifest list: {}", e)))?;
    fs::write(&file_path, json_bytes)
        .map_err(|e| H2Error::Execution(format!("Failed to write manifest list {:?}: {}", file_path, e)))?;
    Ok(format!("metadata/{}", file_name))
}

pub fn read_manifest_file(location: &str, manifest_path: &str) -> H2Result<Vec<ManifestEntry>> {
    let p = resolve_path(location, manifest_path);
    if !p.exists() {
        return Err(H2Error::Execution(format!("Manifest file not found: {:?}", p)));
    }
    let content = fs::read_to_string(&p)
        .map_err(|e| H2Error::Execution(format!("Failed to read manifest file {:?}: {}", p, e)))?;
    let entries: Vec<ManifestEntry> = serde_json::from_str(&content)
        .map_err(|e| H2Error::Execution(format!("Failed to parse manifest file {:?}: {}", p, e)))?;
    Ok(entries)
}

pub fn write_manifest_file(
    location: &str,
    file_name: &str,
    entries: &[ManifestEntry],
) -> H2Result<String> {
    let meta_dir = Path::new(location).join("metadata");
    fs::create_dir_all(&meta_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to create metadata dir: {}", e)))?;
    let file_path = meta_dir.join(file_name);
    let json_bytes = serde_json::to_vec_pretty(entries)
        .map_err(|e| H2Error::Execution(format!("Failed to serialize manifest file: {}", e)))?;
    fs::write(&file_path, json_bytes)
        .map_err(|e| H2Error::Execution(format!("Failed to write manifest file {:?}: {}", file_path, e)))?;
    Ok(format!("metadata/{}", file_name))
}

pub fn init_iceberg_table(location: &str, table_def: &TableDef) -> H2Result<()> {
    let base = Path::new(location);
    let meta_dir = base.join("metadata");
    let data_dir = base.join("data");
    fs::create_dir_all(&meta_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to create metadata directory {:?}: {}", meta_dir, e)))?;
    fs::create_dir_all(&data_dir)
        .map_err(|e| H2Error::Execution(format!("Failed to create data directory {:?}: {}", data_dir, e)))?;

    // 既に version-hint がある場合は再初期化しない
    if read_version_hint(location)?.is_some() {
        return Ok(());
    }

    let schema = create_iceberg_schema(&table_def.columns);
    let now_ms = Utc::now().timestamp_millis();
    let table_uuid = Uuid::new_v4().to_string();

    let metadata = TableMetadata {
        format_version: 2,
        table_uuid,
        location: location.to_string(),
        last_sequence_number: 0,
        last_updated_ms: now_ms,
        last_column_id: table_def.columns.len() as i32,
        current_schema_id: 0,
        schemas: vec![schema],
        default_spec_id: 0,
        partition_specs: vec![PartitionSpec {
            spec_id: 0,
            fields: vec![],
        }],
        last_partition_id: 0,
        current_snapshot_id: None,
        snapshots: vec![],
        snapshot_log: vec![],
    };

    write_table_metadata(location, 1, &metadata)?;
    write_version_hint(location, 1)?;

    Ok(())
}
