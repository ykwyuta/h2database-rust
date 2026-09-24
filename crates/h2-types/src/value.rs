use std::cmp::Ordering;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
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
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn to_decimal(&self) -> Option<Decimal> {
        use rust_decimal::prelude::FromPrimitive;
        match self {
            Value::Decimal(d) => Some(*d),
            Value::TinyInt(n) => Decimal::from_i8(*n),
            Value::SmallInt(n) => Decimal::from_i16(*n),
            Value::Integer(n) => Decimal::from_i32(*n),
            Value::BigInt(n) => Decimal::from_i64(*n),
            Value::Float(f) => Decimal::from_f32(*f),
            Value::Double(d) => Decimal::from_f64(*d),
            _ => None,
        }
    }

    pub fn to_f64(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f as f64),
            Value::Double(d) => Some(*d),
            Value::Decimal(d) => d.to_f64(),
            Value::TinyInt(n) => Some(*n as f64),
            Value::SmallInt(n) => Some(*n as f64),
            Value::Integer(n) => Some(*n as f64),
            Value::BigInt(n) => Some(*n as f64),
            _ => None,
        }
    }

    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Value::TinyInt(n) => Some(*n as i64),
            Value::SmallInt(n) => Some(*n as i64),
            Value::Integer(n) => Some(*n as i64),
            Value::BigInt(n) => Some(*n),
            Value::Decimal(d) => d.to_i64(),
            Value::Float(f) => Some(*f as i64),
            Value::Double(d) => Some(*d as i64),
            _ => None,
        }
    }


    pub fn cast_to(&self, target_type: &DataType) -> H2Result<Value> {
        if self.is_null() {
            return Ok(Value::Null);
        }
        match target_type {
            DataType::TinyInt => {
                if let Some(i) = self.to_i64() {
                    Ok(Value::TinyInt(i as i8))
                } else {
                    Err(H2Error::TypeError(format!("Cannot cast {:?} to TINYINT", self)))
                }
            }
            DataType::SmallInt => {
                if let Some(i) = self.to_i64() {
                    Ok(Value::SmallInt(i as i16))
                } else {
                    Err(H2Error::TypeError(format!("Cannot cast {:?} to SMALLINT", self)))
                }
            }
            DataType::Integer => {
                if let Some(i) = self.to_i64() {
                    Ok(Value::Integer(i as i32))
                } else {
                    Err(H2Error::TypeError(format!("Cannot cast {:?} to INTEGER", self)))
                }
            }
            DataType::BigInt => {
                if let Some(i) = self.to_i64() {
                    Ok(Value::BigInt(i))
                } else {
                    Err(H2Error::TypeError(format!("Cannot cast {:?} to BIGINT", self)))
                }
            }
            DataType::Float => {
                if let Some(f) = self.to_f64() {
                    Ok(Value::Float(f as f32))
                } else {
                    Err(H2Error::TypeError(format!("Cannot cast {:?} to FLOAT", self)))
                }
            }
            DataType::Double => {
                if let Some(f) = self.to_f64() {
                    Ok(Value::Double(f))
                } else {
                    Err(H2Error::TypeError(format!("Cannot cast {:?} to DOUBLE", self)))
                }
            }
            DataType::Decimal(_, _) => {
                if let Some(d) = self.to_decimal() {
                    Ok(Value::Decimal(d))
                } else {
                    Err(H2Error::TypeError(format!("Cannot cast {:?} to DECIMAL", self)))
                }
            }
            DataType::VarChar(_) | DataType::Char(_) => {
                match self {
                    Value::String(s) => Ok(Value::String(s.clone())),
                    _ => Ok(Value::String(self.to_string())),
                }
            }
            DataType::Boolean => {
                match self {
                    Value::Boolean(b) => Ok(Value::Boolean(*b)),
                    _ => Err(H2Error::TypeError(format!("Cannot cast {:?} to BOOLEAN", self))),
                }
            }
            DataType::Json => match self {
                Value::Json(j) => Ok(Value::Json(j.clone())),
                Value::String(s) => match serde_json::from_str(s) {
                    Ok(j) => Ok(Value::Json(j)),
                    Err(e) => Err(H2Error::TypeError(format!("Invalid JSON string: {}", e))),
                },
                _ => Err(H2Error::TypeError(format!("Cannot cast {:?} to JSON", self))),
            },
            _ => Ok(self.clone()),

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
            // 異なる数値型同士の比較 (Cross-type numeric comparison)
            _ => compare_numeric(self, other),
        }
    }
}

fn compare_numeric(a: &Value, b: &Value) -> Option<Ordering> {
    // どちらかがDecimalの場合
    if matches!(a, Value::Decimal(_)) || matches!(b, Value::Decimal(_)) {
        let dec_a = a.to_decimal()?;
        let dec_b = b.to_decimal()?;
        return dec_a.partial_cmp(&dec_b);
    }

    // どちらかが浮動小数点数の場合
    if matches!(a, Value::Float(_) | Value::Double(_)) || matches!(b, Value::Float(_) | Value::Double(_)) {
        let f_a = a.to_f64()?;
        let f_b = b.to_f64()?;
        return f_a.partial_cmp(&f_b);
    }

    // 整数同士の場合
    if let (Some(i_a), Some(i_b)) = (a.to_i64(), b.to_i64()) {
        return i_a.partial_cmp(&i_b);
    }

    None
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
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Boolean(b)
    }
}

impl From<i8> for Value {
    fn from(n: i8) -> Self {
        Value::TinyInt(n)
    }
}

impl From<i16> for Value {
    fn from(n: i16) -> Self {
        Value::SmallInt(n)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Value::Integer(n)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::BigInt(n)
    }
}

impl From<f32> for Value {
    fn from(f: f32) -> Self {
        Value::Float(f)
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Double(f)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<Decimal> for Value {
    fn from(d: Decimal) -> Self {
        Value::Decimal(d)
    }
}

impl From<serde_json::Value> for Value {
    fn from(j: serde_json::Value) -> Self {
        Value::Json(j)
    }
}

