use h2_sql::Row;
use h2_types::Value;
use serde_json::{json, Value as JsonValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Markdown,
    CompactJson,
    Json,
    Csv,
}

impl Default for OutputFormat {
    fn default() -> Self {
        Self::Markdown
    }
}

impl OutputFormat {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "compact_json" | "compact" | "columnar" => Self::CompactJson,
            "json" | "array" => Self::Json,
            "csv" => Self::Csv,
            _ => Self::Markdown,
        }
    }
}

/// クエリ結果を行リストから指定フォーマットの文字列へ変換
pub fn format_query_results(
    columns: &[String],
    rows: &[Row],
    format: OutputFormat,
    is_truncated: bool,
    total_fetched: usize,
) -> String {
    if columns.is_empty() {
        return "Empty result set (0 columns).".to_string();
    }

    match format {
        OutputFormat::Markdown => format_markdown(columns, rows, is_truncated, total_fetched),
        OutputFormat::CompactJson => format_compact_json(columns, rows, is_truncated, total_fetched),
        OutputFormat::Json => format_json(columns, rows, is_truncated, total_fetched),
        OutputFormat::Csv => format_csv(columns, rows, is_truncated, total_fetched),
    }
}

fn format_markdown(columns: &[String], rows: &[Row], is_truncated: bool, total_fetched: usize) -> String {
    let mut out = String::new();

    // 1. Header
    out.push_str("| ");
    out.push_str(&columns.join(" | "));
    out.push_str(" |\n");

    // 2. Separator
    out.push_str("|");
    for _ in columns {
        out.push_str("---|");
    }
    out.push('\n');

    // 3. Rows
    if rows.is_empty() {
        out.push_str("| ");
        out.push_str(&vec!["-"; columns.len()].join(" | "));
        out.push_str(" |\n");
    } else {
        for row in rows {
            out.push_str("| ");
            let formatted_vals: Vec<String> = row
                .values
                .iter()
                .map(format_value_for_markdown)
                .collect();
            out.push_str(&formatted_vals.join(" | "));
            out.push_str(" |\n");
        }
    }

    if is_truncated {
        out.push_str(&format!(
            "\n*⚠️ Result truncated: showing first {} rows (total scanned >= {}).*",
            rows.len(),
            total_fetched
        ));
    }

    out
}

fn format_compact_json(columns: &[String], rows: &[Row], is_truncated: bool, total_fetched: usize) -> String {
    let json_rows: Vec<Vec<JsonValue>> = rows
        .iter()
        .map(|r| r.values.iter().map(value_to_json).collect())
        .collect();

    let mut obj = json!({
        "columns": columns,
        "rows": json_rows,
        "rowCount": rows.len(),
    });

    if is_truncated {
        obj.as_object_mut().unwrap().insert(
            "truncated".to_string(),
            json!({
                "displayed": rows.len(),
                "total": total_fetched
            }),
        );
    }

    serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string())
}

fn format_json(columns: &[String], rows: &[Row], is_truncated: bool, total_fetched: usize) -> String {
    let mut array = Vec::new();
    for row in rows {
        let mut map = serde_json::Map::new();
        for (i, col) in columns.iter().enumerate() {
            let val = row.get(i).unwrap_or(&Value::Null);
            map.insert(col.clone(), value_to_json(val));
        }
        array.push(JsonValue::Object(map));
    }

    let mut root = json!({
        "data": array,
        "count": rows.len(),
    });

    if is_truncated {
        root.as_object_mut().unwrap().insert(
            "truncated".to_string(),
            json!({
                "displayed": rows.len(),
                "total": total_fetched
            }),
        );
    }

    serde_json::to_string_pretty(&root).unwrap_or_else(|_| "[]".to_string())
}

fn format_csv(columns: &[String], rows: &[Row], is_truncated: bool, total_fetched: usize) -> String {
    let mut out = String::new();
    // Headers
    out.push_str(&columns.join(","));
    out.push('\n');

    for row in rows {
        let line: Vec<String> = row.values.iter().map(format_value_for_csv).collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }

    if is_truncated {
        out.push_str(&format!(
            "# Truncated: showing {} rows (total >= {})\n",
            rows.len(),
            total_fetched
        ));
    }

    out
}

fn format_value_for_markdown(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::String(s) => s.replace('|', "\\|").replace('\n', " "),
        Value::Boolean(b) => if *b { "true".to_string() } else { "false".to_string() },
        Value::TinyInt(n) => n.to_string(),
        Value::SmallInt(n) => n.to_string(),
        Value::Integer(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Decimal(d) => d.to_string(),
        Value::Date(d) => d.to_string(),
        Value::Time(t) => t.to_string(),
        Value::Timestamp(ts) => ts.to_string(),
        Value::Uuid(u) => u.to_string(),
        Value::Json(j) => j.to_string().replace('|', "\\|"),
        Value::Bytes(b) => format!("0x{}", hex_encode(b)),
        Value::Array(arr) => format!("[{}]", arr.iter().map(format_value_for_markdown).collect::<Vec<_>>().join(", ")),
        Value::Interval(iv) => iv.to_string(),
    }
}

fn format_value_for_csv(v: &Value) -> String {
    match v {
        Value::Null => "".to_string(),
        Value::String(s) => {
            if s.contains(',') || s.contains('"') || s.contains('\n') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.clone()
            }
        }
        Value::Json(j) => {
            let s = j.to_string();
            format!("\"{}\"", s.replace('"', "\"\""))
        }
        _ => v.to_string(),
    }
}

pub fn value_to_json(v: &Value) -> JsonValue {
    match v {
        Value::Null => JsonValue::Null,
        Value::Boolean(b) => JsonValue::Bool(*b),
        Value::TinyInt(n) => json!(n),
        Value::SmallInt(n) => json!(n),
        Value::Integer(n) => json!(n),
        Value::BigInt(n) => json!(n),
        Value::Float(f) => json!(f),
        Value::Double(d) => json!(d),
        Value::Decimal(d) => JsonValue::String(d.to_string()),
        Value::String(s) => JsonValue::String(s.clone()),
        Value::Date(d) => JsonValue::String(d.to_string()),
        Value::Time(t) => JsonValue::String(t.to_string()),
        Value::Timestamp(ts) => JsonValue::String(ts.to_string()),
        Value::Uuid(u) => JsonValue::String(u.to_string()),
        Value::Json(j) => j.clone(),
        Value::Bytes(b) => JsonValue::String(format!("0x{}", hex_encode(b))),
        Value::Array(arr) => JsonValue::Array(arr.iter().map(value_to_json).collect()),
        Value::Interval(iv) => JsonValue::String(iv.to_string()),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
