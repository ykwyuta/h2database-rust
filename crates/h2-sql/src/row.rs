use serde::{Deserialize, Serialize};
use h2_types::{FromSql, H2Error, H2Result, Value};

/// データベース内の1行
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub values: Vec<Value>,
}

impl Row {
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    pub fn get(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }

    /// 指定したカラムの値を型安全にデシリアライズして取得
    pub fn get_as<T: FromSql>(&self, index: usize) -> H2Result<T> {
        let val = self.values.get(index).ok_or_else(|| {
            H2Error::Execution(format!(
                "Column index {} out of range (row has {} columns)",
                index,
                self.values.len()
            ))
        })?;
        T::from_sql(val)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn to_bytes(&self) -> H2Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(5 + self.values.len() * 16);
        buf.push(0xAA);
        buf.extend_from_slice(&(self.values.len() as u32).to_le_bytes());
        for val in &self.values {
            encode_value(&mut buf, val);
        }
        Ok(buf)
    }

    pub fn from_bytes(bytes: &[u8]) -> H2Result<Self> {
        if bytes.is_empty() {
            return Err(H2Error::Serialization("Empty row bytes".to_string()));
        }
        if bytes[0] != 0xAA || bytes.len() < 5 {
            return Err(H2Error::Serialization("Invalid binary row format: missing magic byte".to_string()));
        }
        let count_bytes = bytes[1..5]
            .try_into()
            .map_err(|_| H2Error::Serialization("Invalid row header".to_string()))?;
        let count = u32::from_le_bytes(count_bytes) as usize;
        let mut values = Vec::with_capacity(count);
        let mut offset = 5;
        for _ in 0..count {
            if let Some((val, consumed)) = decode_value(&bytes[offset..]) {
                values.push(val);
                offset += consumed;
            } else {
                return Err(H2Error::Serialization("Corrupted binary row value".to_string()));
            }
        }
        Ok(Row { values })
    }
}

fn encode_value(buf: &mut Vec<u8>, val: &Value) {
    match val {
        Value::Null => buf.push(0),
        Value::Boolean(b) => {
            buf.push(1);
            buf.push(if *b { 1 } else { 0 });
        }
        Value::TinyInt(i) => {
            buf.push(2);
            buf.push(*i as u8);
        }
        Value::SmallInt(i) => {
            buf.push(3);
            buf.extend_from_slice(&i.to_le_bytes());
        }
        Value::Integer(i) => {
            buf.push(4);
            buf.extend_from_slice(&i.to_le_bytes());
        }
        Value::BigInt(i) => {
            buf.push(5);
            buf.extend_from_slice(&i.to_le_bytes());
        }
        Value::Float(f) => {
            buf.push(6);
            buf.extend_from_slice(&f.to_le_bytes());
        }
        Value::Double(d) => {
            buf.push(7);
            buf.extend_from_slice(&d.to_le_bytes());
        }
        Value::Decimal(dec) => {
            buf.push(8);
            buf.extend_from_slice(&dec.serialize());
        }
        Value::String(s) => {
            buf.push(9);
            let b = s.as_bytes();
            buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
            buf.extend_from_slice(b);
        }
        Value::Bytes(b) => {
            buf.push(10);
            buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
            buf.extend_from_slice(b);
        }
        Value::Date(d) => {
            use chrono::Datelike;
            buf.push(11);
            buf.extend_from_slice(&(d.num_days_from_ce() as i32).to_le_bytes());
        }
        Value::Time(t) => {
            use chrono::Timelike;
            buf.push(12);
            buf.extend_from_slice(&(t.num_seconds_from_midnight() as u32).to_le_bytes());
            buf.extend_from_slice(&t.nanosecond().to_le_bytes());
        }
        Value::Timestamp(ts) => {
            use chrono::Timelike;
            buf.push(13);
            buf.extend_from_slice(&ts.timestamp().to_le_bytes());
            buf.extend_from_slice(&ts.nanosecond().to_le_bytes());
        }
        Value::Uuid(u) => {
            buf.push(14);
            buf.extend_from_slice(u.as_bytes());
        }
        Value::Json(j) => {
            buf.push(15);
            let b = serde_json::to_vec(j).unwrap_or_default();
            buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
            buf.extend_from_slice(&b);
        }
        Value::Array(arr) => {
            buf.push(16);
            buf.extend_from_slice(&(arr.len() as u32).to_le_bytes());
            for v in arr {
                encode_value(buf, v);
            }
        }
        Value::Interval(iv) => {
            buf.push(17);
            buf.extend_from_slice(&iv.months.to_le_bytes());
            buf.extend_from_slice(&iv.days.to_le_bytes());
            buf.extend_from_slice(&iv.microseconds.to_le_bytes());
        }
    }
}

fn decode_value(bytes: &[u8]) -> Option<(Value, usize)> {
    if bytes.is_empty() {
        return None;
    }
    match bytes[0] {
        0 => Some((Value::Null, 1)),
        1 => {
            if bytes.len() < 2 { return None; }
            Some((Value::Boolean(bytes[1] != 0), 2))
        }
        2 => {
            if bytes.len() < 2 { return None; }
            Some((Value::TinyInt(bytes[1] as i8), 2))
        }
        3 => {
            if bytes.len() < 3 { return None; }
            let v = i16::from_le_bytes(bytes[1..3].try_into().ok()?);
            Some((Value::SmallInt(v), 3))
        }
        4 => {
            if bytes.len() < 5 { return None; }
            let v = i32::from_le_bytes(bytes[1..5].try_into().ok()?);
            Some((Value::Integer(v), 5))
        }
        5 => {
            if bytes.len() < 9 { return None; }
            let v = i64::from_le_bytes(bytes[1..9].try_into().ok()?);
            Some((Value::BigInt(v), 9))
        }
        6 => {
            if bytes.len() < 5 { return None; }
            let v = f32::from_le_bytes(bytes[1..5].try_into().ok()?);
            Some((Value::Float(v), 5))
        }
        7 => {
            if bytes.len() < 9 { return None; }
            let v = f64::from_le_bytes(bytes[1..9].try_into().ok()?);
            Some((Value::Double(v), 9))
        }
        8 => {
            if bytes.len() < 17 { return None; }
            let arr: [u8; 16] = bytes[1..17].try_into().ok()?;
            let dec = rust_decimal::Decimal::deserialize(arr);
            Some((Value::Decimal(dec), 17))
        }
        9 => {
            if bytes.len() < 5 { return None; }
            let len = u32::from_le_bytes(bytes[1..5].try_into().ok()?) as usize;
            if bytes.len() < 5 + len { return None; }
            let s = String::from_utf8(bytes[5..5 + len].to_vec()).ok()?;
            Some((Value::String(s), 5 + len))
        }
        10 => {
            if bytes.len() < 5 { return None; }
            let len = u32::from_le_bytes(bytes[1..5].try_into().ok()?) as usize;
            if bytes.len() < 5 + len { return None; }
            let b = bytes[5..5 + len].to_vec();
            Some((Value::Bytes(b), 5 + len))
        }
        11 => {
            if bytes.len() < 5 { return None; }
            let num_days = i32::from_le_bytes(bytes[1..5].try_into().ok()?);
            let d = chrono::NaiveDate::from_num_days_from_ce_opt(num_days)?;
            Some((Value::Date(d), 5))
        }
        12 => {
            if bytes.len() < 9 { return None; }
            let secs = u32::from_le_bytes(bytes[1..5].try_into().ok()?);
            let nano = u32::from_le_bytes(bytes[5..9].try_into().ok()?);
            let t = chrono::NaiveTime::from_num_seconds_from_midnight_opt(secs, nano)?;
            Some((Value::Time(t), 9))
        }
        13 => {
            if bytes.len() < 13 { return None; }
            let secs = i64::from_le_bytes(bytes[1..9].try_into().ok()?);
            let nano = u32::from_le_bytes(bytes[9..13].try_into().ok()?);
            let dt = chrono::DateTime::from_timestamp(secs, nano)?;
            Some((Value::Timestamp(dt), 13))
        }
        14 => {
            if bytes.len() < 17 { return None; }
            let u = uuid::Uuid::from_bytes(bytes[1..17].try_into().ok()?);
            Some((Value::Uuid(u), 17))
        }
        15 => {
            if bytes.len() < 5 { return None; }
            let len = u32::from_le_bytes(bytes[1..5].try_into().ok()?) as usize;
            if bytes.len() < 5 + len { return None; }
            let j: serde_json::Value = serde_json::from_slice(&bytes[5..5 + len]).ok()?;
            Some((Value::Json(j), 5 + len))
        }
        16 => {
            if bytes.len() < 5 { return None; }
            let count = u32::from_le_bytes(bytes[1..5].try_into().ok()?) as usize;
            let mut arr = Vec::with_capacity(count);
            let mut offset = 5;
            for _ in 0..count {
                let (v, consumed) = decode_value(&bytes[offset..])?;
                arr.push(v);
                offset += consumed;
            }
            Some((Value::Array(arr), offset))
        }
        17 => {
            if bytes.len() < 17 { return None; }
            let months = i32::from_le_bytes(bytes[1..5].try_into().ok()?);
            let days = i32::from_le_bytes(bytes[5..9].try_into().ok()?);
            let microseconds = i64::from_le_bytes(bytes[9..17].try_into().ok()?);
            Some((Value::Interval(h2_types::IntervalValue { months, days, microseconds }), 17))
        }
        _ => None,
    }
}

/// 指定カラムの値だけをゼロコピーで抽出（数値の場合は i64/f64 にキャストして返却）
pub fn extract_numeric_column(bytes: &[u8], target_col: usize) -> Option<(i64, f64)> {
    if bytes.len() < 5 || bytes[0] != 0xAA {
        return None;
    }
    let count = u32::from_le_bytes(bytes[1..5].try_into().ok()?) as usize;
    if target_col >= count {
        return None;
    }
    let mut offset = 5;
    for col in 0..=target_col {
        if offset >= bytes.len() {
            return None;
        }
        let tag = bytes[offset];
        let (val_i64, val_f64, size) = match tag {
            0 => (0, 0.0, 1),
            1 => {
                if offset + 2 > bytes.len() { return None; }
                (if bytes[offset + 1] != 0 { 1 } else { 0 }, if bytes[offset + 1] != 0 { 1.0 } else { 0.0 }, 2)
            }
            2 => {
                if offset + 2 > bytes.len() { return None; }
                let v = bytes[offset + 1] as i8 as i64;
                (v, v as f64, 2)
            }
            3 => {
                if offset + 3 > bytes.len() { return None; }
                let v = i16::from_le_bytes(bytes[offset + 1..offset + 3].try_into().ok()?) as i64;
                (v, v as f64, 3)
            }
            4 => {
                if offset + 5 > bytes.len() { return None; }
                let v = i32::from_le_bytes(bytes[offset + 1..offset + 5].try_into().ok()?) as i64;
                (v, v as f64, 5)
            }
            5 => {
                if offset + 9 > bytes.len() { return None; }
                let v = i64::from_le_bytes(bytes[offset + 1..offset + 9].try_into().ok()?);
                (v, v as f64, 9)
            }
            6 => {
                if offset + 5 > bytes.len() { return None; }
                let v = f32::from_le_bytes(bytes[offset + 1..offset + 5].try_into().ok()?);
                (v as i64, v as f64, 5)
            }
            7 => {
                if offset + 9 > bytes.len() { return None; }
                let v = f64::from_le_bytes(bytes[offset + 1..offset + 9].try_into().ok()?);
                (v as i64, v, 9)
            }
            9 | 10 | 14 | 15 => {
                if offset + 5 > bytes.len() { return None; }
                let len = u32::from_le_bytes(bytes[offset + 1..offset + 5].try_into().ok()?) as usize;
                (0, 0.0, 5 + len)
            }
            8 => (0, 0.0, 17),
            11 => (0, 0.0, 5),
            12 => (0, 0.0, 9),
            13 => (0, 0.0, 13),
            _ => return None,
        };
        if col == target_col {
            if tag == 0 {
                return None; // Null
            }
            return Some((val_i64, val_f64));
        }
        offset += size;
    }
    None
}
