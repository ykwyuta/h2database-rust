use std::sync::Arc;
use arrow::array::*;
use arrow::datatypes::{DataType as ArrowDataType, Field, Schema, SchemaRef, TimeUnit};
use arrow::record_batch::RecordBatch;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use std::str::FromStr;

use h2_types::{DataType as H2DataType, H2Error, H2Result, Value};
use crate::catalog::ColumnDef;
use crate::row::Row;

/// H2 のデータ型を Apache Arrow のデータ型へマッピング
pub fn h2_type_to_arrow_type(dt: &H2DataType) -> ArrowDataType {
    match dt {
        H2DataType::Boolean => ArrowDataType::Boolean,
        H2DataType::TinyInt => ArrowDataType::Int8,
        H2DataType::SmallInt => ArrowDataType::Int16,
        H2DataType::Integer => ArrowDataType::Int32,
        H2DataType::BigInt => ArrowDataType::Int64,
        H2DataType::Float => ArrowDataType::Float32,
        H2DataType::Double => ArrowDataType::Float64,
        H2DataType::Decimal(p, s) => ArrowDataType::Decimal128(*p, *s as i8),
        H2DataType::Char(_) | H2DataType::VarChar(_) => ArrowDataType::Utf8,
        H2DataType::Binary(_) | H2DataType::Blob => ArrowDataType::Binary,
        H2DataType::Date => ArrowDataType::Date32,
        H2DataType::Time => ArrowDataType::Time64(TimeUnit::Microsecond),
        H2DataType::Timestamp | H2DataType::TimestampTz => {
            ArrowDataType::Timestamp(TimeUnit::Microsecond, None)
        }
        H2DataType::Uuid => ArrowDataType::Utf8,
        H2DataType::Json => ArrowDataType::Utf8,
        H2DataType::Interval => ArrowDataType::Utf8,
        H2DataType::Array(_) => ArrowDataType::Utf8,
    }
}

/// H2 カラム定義リストから Arrow の Schema を生成
pub fn create_arrow_schema(columns: &[ColumnDef]) -> SchemaRef {
    let fields: Vec<Field> = columns
        .iter()
        .map(|c| {
            let arrow_type = h2_type_to_arrow_type(&c.data_type);
            Field::new(&c.name, arrow_type, c.is_nullable)
        })
        .collect();
    Arc::new(Schema::new(fields))
}

/// 複数の Row から Apache Arrow の RecordBatch を構築する
pub fn rows_to_record_batch(
    schema: &SchemaRef,
    rows: &[Row],
) -> H2Result<RecordBatch> {
    if rows.is_empty() {
        return Ok(RecordBatch::new_empty(schema.clone()));
    }

    let num_cols = schema.fields().len();
    let num_rows = rows.len();
    let mut columns: Vec<ArrayRef> = Vec::with_capacity(num_cols);

    for (col_idx, field) in schema.fields().iter().enumerate() {
        let array: ArrayRef = match field.data_type() {
            ArrowDataType::Boolean => {
                let mut builder = BooleanBuilder::with_capacity(num_rows);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::Boolean(b)) => builder.append_value(*b),
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected bool, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Int8 => {
                let mut builder = Int8Builder::with_capacity(num_rows);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::TinyInt(v)) => builder.append_value(*v),
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected i8, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Int16 => {
                let mut builder = Int16Builder::with_capacity(num_rows);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::SmallInt(v)) => builder.append_value(*v),
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected i16, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Int32 => {
                let mut builder = Int32Builder::with_capacity(num_rows);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::Integer(v)) => builder.append_value(*v),
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected i32, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Int64 => {
                let mut builder = Int64Builder::with_capacity(num_rows);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::BigInt(v)) => builder.append_value(*v),
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected i64, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Float32 => {
                let mut builder = Float32Builder::with_capacity(num_rows);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::Float(v)) => builder.append_value(*v),
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected f32, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Float64 => {
                let mut builder = Float64Builder::with_capacity(num_rows);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::Double(v)) => builder.append_value(*v),
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected f64, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Utf8 => {
                let mut builder = StringBuilder::with_capacity(num_rows, num_rows * 16);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::String(s)) => builder.append_value(s),
                        Some(Value::Uuid(u)) => builder.append_value(u.to_string()),
                        Some(Value::Json(j)) => builder.append_value(j.to_string()),
                        Some(Value::Interval(i)) => builder.append_value(format!("{:?}", i)),
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => builder.append_value(format!("{}", other)),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Binary => {
                let mut builder = BinaryBuilder::with_capacity(num_rows, num_rows * 32);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::Bytes(b)) => builder.append_value(b),
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected bytes, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Date32 => {
                let mut builder = Date32Builder::with_capacity(num_rows);
                let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::Date(d)) => {
                            let days = (*d - epoch).num_days() as i32;
                            builder.append_value(days);
                        }
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected Date, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Timestamp(TimeUnit::Microsecond, _) => {
                let mut builder = TimestampMicrosecondBuilder::with_capacity(num_rows);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::Timestamp(ts)) => {
                            builder.append_value(ts.timestamp_micros());
                        }
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected Timestamp, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            ArrowDataType::Decimal128(p, s) => {
                let mut builder = Decimal128Builder::with_capacity(num_rows)
                    .with_precision_and_scale(*p, *s)
                    .map_err(|e| H2Error::Execution(format!("Decimal128Builder error: {}", e)))?;
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::Decimal(d)) => {
                            let d_res = d.to_string();
                            if let Ok(v) = d_res.parse::<f64>() {
                                let scaled = (v * (10_f64).powi(*s as i32)).round() as i128;
                                builder.append_value(scaled);
                            } else {
                                builder.append_null();
                            }
                        }
                        Some(Value::Null) | None => builder.append_null(),
                        Some(other) => return Err(H2Error::TypeError(format!("Expected Decimal, got {:?}", other))),
                    }
                }
                Arc::new(builder.finish())
            }
            _ => {
                // その他の型はフォールバックとして文字列表現で格納
                let mut builder = StringBuilder::with_capacity(num_rows, num_rows * 16);
                for row in rows {
                    match row.get(col_idx) {
                        Some(Value::Null) | None => builder.append_null(),
                        Some(val) => builder.append_value(format!("{}", val)),
                    }
                }
                Arc::new(builder.finish())
            }
        };
        columns.push(array);
    }

    RecordBatch::try_new(schema.clone(), columns)
        .map_err(|e| H2Error::Execution(format!("Failed to build RecordBatch: {}", e)))
}

/// Apache Arrow の RecordBatch から指定された行インデックスの Row を復元
pub fn record_batch_to_row(batch: &RecordBatch, row_idx: usize) -> H2Result<Row> {
    if row_idx >= batch.num_rows() {
        return Err(H2Error::Execution(format!(
            "Row index out of bounds: {} >= {}",
            row_idx,
            batch.num_rows()
        )));
    }

    let mut values = Vec::with_capacity(batch.num_columns());

    for column in batch.columns() {
        if column.is_null(row_idx) {
            values.push(Value::Null);
            continue;
        }

        let val = match column.data_type() {
            ArrowDataType::Boolean => {
                let array = column.as_any().downcast_ref::<BooleanArray>().unwrap();
                Value::Boolean(array.value(row_idx))
            }
            ArrowDataType::Int8 => {
                let array = column.as_any().downcast_ref::<Int8Array>().unwrap();
                Value::TinyInt(array.value(row_idx))
            }
            ArrowDataType::Int16 => {
                let array = column.as_any().downcast_ref::<Int16Array>().unwrap();
                Value::SmallInt(array.value(row_idx))
            }
            ArrowDataType::Int32 => {
                let array = column.as_any().downcast_ref::<Int32Array>().unwrap();
                Value::Integer(array.value(row_idx))
            }
            ArrowDataType::Int64 => {
                let array = column.as_any().downcast_ref::<Int64Array>().unwrap();
                Value::BigInt(array.value(row_idx))
            }
            ArrowDataType::Float32 => {
                let array = column.as_any().downcast_ref::<Float32Array>().unwrap();
                Value::Float(array.value(row_idx))
            }
            ArrowDataType::Float64 => {
                let array = column.as_any().downcast_ref::<Float64Array>().unwrap();
                Value::Double(array.value(row_idx))
            }
            ArrowDataType::Utf8 => {
                let array = column.as_any().downcast_ref::<StringArray>().unwrap();
                Value::String(array.value(row_idx).to_string())
            }
            ArrowDataType::Binary => {
                let array = column.as_any().downcast_ref::<BinaryArray>().unwrap();
                Value::Bytes(array.value(row_idx).to_vec())
            }
            ArrowDataType::Date32 => {
                let array = column.as_any().downcast_ref::<Date32Array>().unwrap();
                let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
                let days = array.value(row_idx);
                let date = epoch + chrono::Duration::days(days as i64);
                Value::Date(date)
            }
            ArrowDataType::Timestamp(TimeUnit::Microsecond, _) => {
                let array = column.as_any().downcast_ref::<TimestampMicrosecondArray>().unwrap();
                let micros = array.value(row_idx);
                let dt = DateTime::from_timestamp_micros(micros).unwrap_or_else(Utc::now);
                Value::Timestamp(dt)
            }
            ArrowDataType::Decimal128(_, s) => {
                let array = column.as_any().downcast_ref::<Decimal128Array>().unwrap();
                let v = array.value(row_idx);
                let divisor = 10_i128.pow(*s as u32);
                let int_part = v / divisor;
                let frac_part = (v % divisor).abs();
                let str_repr = format!("{}.{:0width$}", int_part, frac_part, width = *s as usize);
                Decimal::from_str(&str_repr)
                    .map(Value::Decimal)
                    .unwrap_or(Value::Null)
            }
            _ => {
                // 不明な型は文字列として取得を試みる
                if let Some(array) = column.as_any().downcast_ref::<StringArray>() {
                    Value::String(array.value(row_idx).to_string())
                } else {
                    Value::Null
                }
            }
        };
        values.push(val);
    }

    Ok(Row::new(values))
}
