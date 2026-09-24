use std::cmp::Ordering;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::data_type::DataType;
use crate::error::{H2Error, H2Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Boolean(bool),
    TinyInt(i8),
    SmallInt(i16),
    Integer(i32),
    BigInt(i64),
    Float(f32),
    Double(f64),
    Decimal(Decimal),
    String(String),
    Bytes(Vec<u8>),
    Date(NaiveDate),
    Time(NaiveTime),
    Timestamp(DateTime<Utc>),
    Uuid(Uuid),
    Json(serde_json::Value),
    Array(Vec<Value>),
    Vector(Vec<f32>),
}

impl Value {
    pub fn data_type(&self) -> Option<DataType> {
        match self {
            Value::Null => None,
            Value::Boolean(_) => Some(DataType::Boolean),
            Value::TinyInt(_) => Some(DataType::TinyInt),
            Value::SmallInt(_) => Some(DataType::SmallInt),
            Value::Integer(_) => Some(DataType::Integer),
            Value::BigInt(_) => Some(DataType::BigInt),
            Value::Float(_) => Some(DataType::Float),
            Value::Double(_) => Some(DataType::Double),
            Value::Decimal(d) => Some(DataType::Decimal(d.mantissa().to_string().len() as u8, d.scale() as u8)),
            Value::String(_) => Some(DataType::VarChar(None)),
            Value::Bytes(_) => Some(DataType::Blob),
            Value::Date(_) => Some(DataType::Date),
            Value::Time(_) => Some(DataType::Time),
            Value::Timestamp(_) => Some(DataType::TimestampTz),
            Value::Uuid(_) => Some(DataType::Uuid),
            Value::Json(_) => Some(DataType::Json),
            Value::Array(items) => {
                let inner = items.first().and_then(|v| v.data_type()).unwrap_or(DataType::Integer);
                Some(DataType::Array(Box::new(inner)))
            }
            Value::Vector(v) => Some(DataType::Vector(v.len())),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// ベクトル同士のコサイン類似度計算
    pub fn cosine_similarity(&self, other: &Value) -> H2Result<f32> {
        match (self, other) {
            (Value::Vector(v1), Value::Vector(v2)) => {
                if v1.len() != v2.len() {
                    return Err(H2Error::TypeError(format!(
                        "Vector dimensions mismatch: {} vs {}",
                        v1.len(),
                        v2.len()
                    )));
                }
                let mut dot_product = 0.0f32;
                let mut norm_a = 0.0f32;
                let mut norm_b = 0.0f32;
                for (&a, &b) in v1.iter().zip(v2.iter()) {
                    dot_product += a * b;
                    norm_a += a * a;
                    norm_b += b * b;
                }
                let denom = norm_a.sqrt() * norm_b.sqrt();
                if denom == 0.0 {
                    Ok(0.0)
                } else {
                    Ok(dot_product / denom)
                }
            }
            _ => Err(H2Error::TypeError("Cosine similarity requires Vector values".to_string())),
        }
    }
}

/// インデックスキー比較用のPartialOrd実装
impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Value::Null, Value::Null) => Some(Ordering::Equal),
            (Value::Null, _) => Some(Ordering::Less),
            (_, Value::Null) => Some(Ordering::Greater),
            (Value::Boolean(a), Value::Boolean(b)) => a.partial_cmp(b),
            (Value::TinyInt(a), Value::TinyInt(b)) => a.partial_cmp(b),
            (Value::SmallInt(a), Value::SmallInt(b)) => a.partial_cmp(b),
            (Value::Integer(a), Value::Integer(b)) => a.partial_cmp(b),
            (Value::BigInt(a), Value::BigInt(b)) => a.partial_cmp(b),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Double(a), Value::Double(b)) => a.partial_cmp(b),
            (Value::Decimal(a), Value::Decimal(b)) => a.partial_cmp(b),
            (Value::String(a), Value::String(b)) => a.partial_cmp(b),
            (Value::Bytes(a), Value::Bytes(b)) => a.partial_cmp(b),
            (Value::Date(a), Value::Date(b)) => a.partial_cmp(b),
            (Value::Time(a), Value::Time(b)) => a.partial_cmp(b),
            (Value::Timestamp(a), Value::Timestamp(b)) => a.partial_cmp(b),
            (Value::Uuid(a), Value::Uuid(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Boolean(b) => write!(f, "{}", b),
            Value::TinyInt(n) => write!(f, "{}", n),
            Value::SmallInt(n) => write!(f, "{}", n),
            Value::Integer(n) => write!(f, "{}", n),
            Value::BigInt(n) => write!(f, "{}", n),
            Value::Float(n) => write!(f, "{}", n),
            Value::Double(n) => write!(f, "{}", n),
            Value::Decimal(d) => write!(f, "{}", d),
            Value::String(s) => write!(f, "'{}'", s.replace('\'', "''")),
            Value::Bytes(b) => {
                write!(f, "X'")?;
                for byte in b {
                    write!(f, "{:02X}", byte)?;
                }
                write!(f, "'")
            }
            Value::Date(d) => write!(f, "{}", d),
            Value::Time(t) => write!(f, "{}", t),
            Value::Timestamp(ts) => write!(f, "{}", ts),
            Value::Uuid(u) => write!(f, "{}", u),
            Value::Json(j) => write!(f, "{}", j),
            Value::Array(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, "]")
            }
            Value::Vector(v) => write!(f, "{:?}", v),
        }
    }
}
