use std::cmp::Ordering;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};


use uuid::Uuid;

use crate::data_type::DataType;
use crate::error::{H2Error, H2Result};


#[derive(Debug, Clone, Serialize, Deserialize)]
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
    Interval(IntervalValue),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
pub struct IntervalValue {
    pub months: i32,
    pub days: i32,
    pub microseconds: i64,
}

impl IntervalValue {
    pub fn new(months: i32, days: i32, microseconds: i64) -> Self {
        Self { months, days, microseconds }
    }

    pub fn from_days(days: i32) -> Self {
        Self { months: 0, days, microseconds: 0 }
    }

    pub fn from_hours(hours: i64) -> Self {
        Self { months: 0, days: 0, microseconds: hours * 3_600_000_000 }
    }

    pub fn from_minutes(mins: i64) -> Self {
        Self { months: 0, days: 0, microseconds: mins * 60_000_000 }
    }

    pub fn from_seconds(secs: i64) -> Self {
        Self { months: 0, days: 0, microseconds: secs * 1_000_000 }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let mut months = 0i32;
        let mut days = 0i32;
        let mut microseconds = 0i64;

        let parts: Vec<&str> = s.split_whitespace().collect();
        let mut i = 0;
        while i < parts.len() {
            let part = parts[i];
            if part.contains(':') {
                let time_parts: Vec<&str> = part.split(':').collect();
                if time_parts.len() >= 2 {
                    let h: i64 = time_parts[0].parse().ok()?;
                    let m: i64 = time_parts[1].parse().ok()?;
                    let s_f: f64 = if time_parts.len() >= 3 { time_parts[2].parse().ok()? } else { 0.0 };
                    microseconds += h * 3_600_000_000 + m * 60_000_000 + (s_f * 1_000_000.0) as i64;
                    i += 1;
                    continue;
                }
            }

            if let Ok(num) = part.parse::<f64>() {
                if i + 1 < parts.len() {
                    let unit = parts[i + 1].to_lowercase();
                    let unit = unit.trim_end_matches('s');
                    match unit {
                        "year" => months += (num * 12.0) as i32,
                        "month" => months += num as i32,
                        "week" => days += (num * 7.0) as i32,
                        "day" => days += num as i32,
                        "hour" => microseconds += (num * 3_600_000_000.0) as i64,
                        "minute" => microseconds += (num * 60_000_000.0) as i64,
                        "second" => microseconds += (num * 1_000_000.0) as i64,
                        "millisecond" | "msec" => microseconds += (num * 1_000.0) as i64,
                        "microsecond" | "usec" => microseconds += num as i64,
                        _ => return None,
                    }
                    i += 2;
                    continue;
                }
            }
            return None;
        }

        Some(Self { months, days, microseconds })
    }
}

impl std::fmt::Display for IntervalValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        let years = self.months / 12;
        let months = self.months % 12;
        if years != 0 {
            parts.push(format!("{} year{}", years, if years.abs() > 1 { "s" } else { "" }));
        }
        if months != 0 {
            parts.push(format!("{} mon{}", months, if months.abs() > 1 { "s" } else { "" }));
        }
        if self.days != 0 {
            parts.push(format!("{} day{}", self.days, if self.days.abs() > 1 { "s" } else { "" }));
        }
        if self.microseconds != 0 || parts.is_empty() {
            let total_secs = self.microseconds as f64 / 1_000_000.0;
            let secs = (total_secs.abs() % 60.0) as u64;
            let mins = ((total_secs.abs() / 60.0) as u64) % 60;
            let hours = (total_secs.abs() / 3600.0) as u64;
            let sign = if self.microseconds < 0 { "-" } else { "" };
            parts.push(format!("{}{:02}:{:02}:{:02}", sign, hours, mins, secs));
        }
        write!(f, "{}", parts.join(" "))
    }
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
            Value::Interval(_) => Some(DataType::Interval),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Value::TinyInt(_)
                | Value::SmallInt(_)
                | Value::Integer(_)
                | Value::BigInt(_)
                | Value::Float(_)
                | Value::Double(_)
                | Value::Decimal(_)
        )
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
            Value::String(s) => s.trim().parse::<f64>().ok(),
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
            Value::String(s) => s.trim().parse::<i64>().ok(),
            _ => None,
        }
    }

    pub fn to_sql_literal(&self) -> String {
        match self {
            Value::Null => "NULL".to_string(),
            Value::Boolean(b) => if *b { "TRUE".to_string() } else { "FALSE".to_string() },
            Value::TinyInt(n) => n.to_string(),
            Value::SmallInt(n) => n.to_string(),
            Value::Integer(n) => n.to_string(),
            Value::BigInt(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Double(d) => d.to_string(),
            Value::Decimal(d) => d.to_string(),
            Value::String(s) => format!("'{}'", s.replace('\'', "''")),
            Value::Bytes(b) => format!("'{}'", String::from_utf8_lossy(b).replace('\'', "''")),
            Value::Date(d) => format!("DATE '{}'", d.format("%Y-%m-%d")),
            Value::Time(t) => format!("TIME '{}'", t.format("%H:%M:%S")),
            Value::Timestamp(ts) => format!("TIMESTAMP '{}'", ts.format("%Y-%m-%d %H:%M:%S")),
            Value::Uuid(u) => format!("'{}'", u),
            Value::Json(j) => format!("'{}'", j.to_string().replace('\'', "''")),
            Value::Interval(inv) => format!("INTERVAL '{}'", inv),
            Value::Array(arr) => format!("ARRAY[{}]", arr.iter().map(|v| v.to_sql_literal()).collect::<Vec<_>>().join(", ")),
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
                    Value::String(s) => match s.trim().to_uppercase().as_str() {
                        "TRUE" | "1" | "T" | "YES" => Ok(Value::Boolean(true)),
                        "FALSE" | "0" | "F" | "NO" => Ok(Value::Boolean(false)),
                        _ => Err(H2Error::TypeError(format!("Cannot cast '{}' to BOOLEAN", s))),
                    },
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
            DataType::Timestamp | DataType::TimestampTz => match self {
                Value::Timestamp(ts) => Ok(Value::Timestamp(*ts)),
                Value::String(s) => {
                    if let Some(ts) = parse_timestamp(s) {
                        Ok(Value::Timestamp(ts))
                    } else {
                        Err(H2Error::TypeError(format!("Cannot parse '{}' as TIMESTAMP / TIMESTAMPTZ", s)))
                    }
                }
                Value::Date(d) => {
                    if let Some(ndt) = d.and_hms_opt(0, 0, 0) {
                        Ok(Value::Timestamp(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc)))
                    } else {
                        Err(H2Error::TypeError(format!("Cannot cast date {:?} to TIMESTAMP", d)))
                    }
                }
                _ => Err(H2Error::TypeError(format!("Cannot cast {:?} to TIMESTAMP / TIMESTAMPTZ", self))),
            },
            DataType::Date => match self {
                Value::Date(d) => Ok(Value::Date(*d)),
                Value::Timestamp(ts) => Ok(Value::Date(ts.date_naive())),
                Value::String(s) => {
                    if let Ok(d) = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d") {
                        Ok(Value::Date(d))
                    } else {
                        Err(H2Error::TypeError(format!("Cannot parse '{}' as DATE", s)))
                    }
                }
                _ => Err(H2Error::TypeError(format!("Cannot cast {:?} to DATE", self))),
            },
            DataType::Time => match self {
                Value::Time(t) => Ok(Value::Time(*t)),
                Value::Timestamp(ts) => Ok(Value::Time(ts.time())),
                Value::String(s) => {
                    if let Ok(t) = chrono::NaiveTime::parse_from_str(s.trim(), "%H:%M:%S") {
                        Ok(Value::Time(t))
                    } else {
                        Err(H2Error::TypeError(format!("Cannot parse '{}' as TIME", s)))
                    }
                }
                _ => Err(H2Error::TypeError(format!("Cannot cast {:?} to TIME", self))),
            },
            DataType::Uuid => match self {
                Value::Uuid(u) => Ok(Value::Uuid(*u)),
                Value::String(s) => {
                    if let Ok(u) = uuid::Uuid::parse_str(s.trim()) {
                        Ok(Value::Uuid(u))
                    } else {
                        Err(H2Error::TypeError(format!("Cannot parse '{}' as UUID", s)))
                    }
                }
                _ => Err(H2Error::TypeError(format!("Cannot cast {:?} to UUID", self))),
            },
            DataType::Interval => match self {
                Value::Interval(iv) => Ok(Value::Interval(*iv)),
                Value::String(s) => {
                    if let Some(iv) = IntervalValue::parse(s) {
                        Ok(Value::Interval(iv))
                    } else {
                        Err(H2Error::TypeError(format!("Cannot parse '{}' as INTERVAL", s)))
                    }
                }
                _ => Err(H2Error::TypeError(format!("Cannot cast {:?} to INTERVAL", self))),
            },
            _ => Ok(self.clone()),
        }
    }
}

fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    let trimmed = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.with_timezone(&Utc));
    }
    let formats_with_tz = [
        "%Y-%m-%d %H:%M:%S%z",
        "%Y-%m-%d %H:%M:%S %#z",
        "%Y-%m-%d %H:%M:%S%:z",
        "%Y-%m-%dT%H:%M:%S%:z",
    ];
    for fmt in &formats_with_tz {
        if let Ok(dt) = DateTime::parse_from_str(trimmed, fmt) {
            return Some(dt.with_timezone(&Utc));
        }
    }
    let naive_formats = [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ];
    for fmt in &naive_formats {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Some(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
        }
    }
    if let Ok(nd) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        if let Some(ndt) = nd.and_hms_opt(0, 0, 0) {
            return Some(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
        }
    }
    None
}



impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Boolean(a), Value::Boolean(b)) => a == b,
            (Value::TinyInt(a), Value::TinyInt(b)) => a == b,
            (Value::SmallInt(a), Value::SmallInt(b)) => a == b,
            (Value::Integer(a), Value::Integer(b)) => a == b,
            (Value::BigInt(a), Value::BigInt(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Double(a), Value::Double(b)) => a == b,
            (Value::Decimal(a), Value::Decimal(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::Date(a), Value::Date(b)) => a == b,
            (Value::Time(a), Value::Time(b)) => a == b,
            (Value::Timestamp(a), Value::Timestamp(b)) => a == b,
            (Value::Uuid(a), Value::Uuid(b)) => a == b,
            (Value::Json(a), Value::Json(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            // 異なる数値型同士の等価判定
            (a, b) if a.is_numeric() && b.is_numeric() => {
                compare_numeric(a, b) == Some(Ordering::Equal)
            }
            // Timestamp と String のクロス等価判定
            (Value::Timestamp(a), Value::String(s)) | (Value::String(s), Value::Timestamp(a)) => {
                if let Some(b) = parse_timestamp(s) {
                    *a == b
                } else {
                    false
                }
            }
            // Date と String のクロス等価判定
            (Value::Date(a), Value::String(s)) | (Value::String(s), Value::Date(a)) => {
                if let Ok(b) = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d") {
                    *a == b
                } else {
                    false
                }
            }
            _ => false,
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
            (Value::Array(a), Value::Array(b)) => a.partial_cmp(b),
            (Value::Interval(a), Value::Interval(b)) => a.partial_cmp(b),
            // Timestamp と String のクロス比較
            (Value::Timestamp(a), Value::String(s)) => {
                let b = parse_timestamp(s)?;
                a.partial_cmp(&b)
            }
            (Value::String(s), Value::Timestamp(b)) => {
                let a = parse_timestamp(s)?;
                a.partial_cmp(b)
            }
            // Date と String のクロス比較
            (Value::Date(a), Value::String(s)) => {
                let b = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()?;
                a.partial_cmp(&b)
            }
            (Value::String(s), Value::Date(b)) => {
                let a = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()?;
                a.partial_cmp(b)
            }
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
            Value::Interval(iv) => write!(f, "{}", iv),
        }
    }
}

impl From<IntervalValue> for Value {
    fn from(iv: IntervalValue) -> Self {
        Value::Interval(iv)
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

impl From<Uuid> for Value {
    fn from(u: Uuid) -> Self {
        Value::Uuid(u)
    }
}

/// SQL クエリ結果の Value から Rust のネイティブ型へ型安全にデシリアライズするトレイト
pub trait FromSql: Sized {
    fn from_sql(val: &Value) -> H2Result<Self>;
}

impl<T: FromSql> FromSql for Option<T> {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::Null => Ok(None),
            other => T::from_sql(other).map(Some),
        }
    }
}

macro_rules! impl_from_sql_int {
    ($($t:ty),*) => {
        $(
            impl FromSql for $t {
                fn from_sql(val: &Value) -> H2Result<Self> {
                    let i = val.to_i64().ok_or_else(|| {
                        H2Error::TypeError(format!("Expected integer, found {:?}", val))
                    })?;
                    Self::try_from(i).map_err(|e| {
                        H2Error::TypeError(format!("Value {} out of range: {}", i, e))
                    })
                }
            }
        )*
    };
}

impl_from_sql_int!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl FromSql for f32 {
    fn from_sql(val: &Value) -> H2Result<Self> {
        let f = val.to_f64().ok_or_else(|| {
            H2Error::TypeError(format!("Expected float, found {:?}", val))
        })?;
        Ok(f as f32)
    }
}

impl FromSql for f64 {
    fn from_sql(val: &Value) -> H2Result<Self> {
        val.to_f64().ok_or_else(|| {
            H2Error::TypeError(format!("Expected double/float, found {:?}", val))
        })
    }
}

impl FromSql for bool {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::Boolean(b) => Ok(*b),
            Value::TinyInt(n) => Ok(*n != 0),
            Value::SmallInt(n) => Ok(*n != 0),
            Value::Integer(n) => Ok(*n != 0),
            Value::BigInt(n) => Ok(*n != 0),
            _ => Err(H2Error::TypeError(format!("Expected boolean, found {:?}", val))),
        }
    }
}

impl FromSql for String {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::String(s) => Ok(s.clone()),
            Value::Json(j) => Ok(j.to_string()),
            Value::Uuid(u) => Ok(u.to_string()),
            Value::Null => Err(H2Error::TypeError("Expected string, found NULL".to_string())),
            other => Ok(other.to_string()),
        }
    }
}

impl FromSql for Vec<u8> {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::Bytes(b) => Ok(b.clone()),
            Value::String(s) => Ok(s.as_bytes().to_vec()),
            _ => Err(H2Error::TypeError(format!("Expected binary/bytes, found {:?}", val))),
        }
    }
}

impl FromSql for Decimal {
    fn from_sql(val: &Value) -> H2Result<Self> {
        val.to_decimal().ok_or_else(|| {
            H2Error::TypeError(format!("Expected decimal, found {:?}", val))
        })
    }
}

impl FromSql for Uuid {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::Uuid(u) => Ok(*u),
            Value::String(s) => Uuid::parse_str(s).map_err(|e| {
                H2Error::TypeError(format!("Invalid UUID string '{}': {}", s, e))
            }),
            _ => Err(H2Error::TypeError(format!("Expected UUID, found {:?}", val))),
        }
    }
}

impl FromSql for chrono::NaiveDate {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::Date(d) => Ok(*d),
            Value::String(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| {
                H2Error::TypeError(format!("Invalid date format '{}': {}", s, e))
            }),
            _ => Err(H2Error::TypeError(format!("Expected Date, found {:?}", val))),
        }
    }
}

impl FromSql for chrono::NaiveTime {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::Time(t) => Ok(*t),
            Value::String(s) => chrono::NaiveTime::parse_from_str(s, "%H:%M:%S").map_err(|e| {
                H2Error::TypeError(format!("Invalid time format '{}': {}", s, e))
            }),
            _ => Err(H2Error::TypeError(format!("Expected Time, found {:?}", val))),
        }
    }
}

impl FromSql for chrono::NaiveDateTime {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::Timestamp(dt) => Ok(dt.naive_utc()),
            Value::String(s) => chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").map_err(|e| {
                H2Error::TypeError(format!("Invalid timestamp format '{}': {}", s, e))
            }),
            _ => Err(H2Error::TypeError(format!("Expected Timestamp, found {:?}", val))),
        }
    }
}

impl FromSql for chrono::DateTime<chrono::Utc> {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::Timestamp(dt) => Ok(*dt),
            _ => Err(H2Error::TypeError(format!("Expected Timestamp, found {:?}", val))),
        }
    }
}

impl FromSql for serde_json::Value {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::Json(j) => Ok(j.clone()),
            Value::String(s) => serde_json::from_str(s).map_err(|e| {
                H2Error::TypeError(format!("Invalid JSON string '{}': {}", s, e))
            }),
            _ => Err(H2Error::TypeError(format!("Expected JSON, found {:?}", val))),
        }
    }
}

impl FromSql for Value {
    fn from_sql(val: &Value) -> H2Result<Self> {
        Ok(val.clone())
    }
}

impl FromSql for IntervalValue {
    fn from_sql(val: &Value) -> H2Result<Self> {
        match val {
            Value::Interval(iv) => Ok(*iv),
            Value::String(s) => IntervalValue::parse(s).ok_or_else(|| {
                H2Error::TypeError(format!("Invalid interval string '{}'", s))
            }),
            _ => Err(H2Error::TypeError(format!("Expected Interval, found {:?}", val))),
        }
    }
}

