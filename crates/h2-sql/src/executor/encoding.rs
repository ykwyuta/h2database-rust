use chrono::Datelike;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use h2_types::Value;
use crate::catalog::{IndexDef, TableDef};
use crate::row::Row;

pub(crate) fn normalize_object_name(name: &sqlparser::ast::ObjectName) -> String {
    name.0.iter().map(|ident| ident.value.clone()).collect::<Vec<_>>().join(".")
}

pub(crate) fn encode_memcomparable_value(val: &Value, buf: &mut Vec<u8>) {
    match val {
        Value::Null => {
            buf.push(0x01);
        }
        Value::Boolean(b) => {
            buf.push(0x02);
            buf.push(if *b { 1 } else { 0 });
        }
        Value::TinyInt(i) => {
            buf.push(0x03);
            buf.push((*i as u8) ^ 0x80);
        }
        Value::SmallInt(i) => {
            buf.push(0x04);
            buf.extend_from_slice(&((*i as u16) ^ 0x8000).to_be_bytes());
        }
        Value::Integer(i) => {
            buf.push(0x05);
            buf.extend_from_slice(&((*i as u32) ^ 0x8000_0000).to_be_bytes());
        }
        Value::BigInt(i) => {
            buf.push(0x06);
            buf.extend_from_slice(&((*i as u64) ^ 0x8000_0000_0000_0000).to_be_bytes());
        }
        Value::Float(f) => {
            buf.push(0x07);
            let u = f.to_bits();
            let encoded = if (u & 0x8000_0000) != 0 {
                !u
            } else {
                u ^ 0x8000_0000
            };
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::Double(f) => {
            buf.push(0x08);
            let u = f.to_bits();
            let encoded = if (u & 0x8000_0000_0000_0000) != 0 {
                !u
            } else {
                u ^ 0x8000_0000_0000_0000
            };
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::String(s) => {
            buf.push(0x09);
            for &byte in s.as_bytes() {
                buf.push(byte);
                if byte == 0x00 {
                    buf.push(0xFF);
                }
            }
            buf.push(0x00);
            buf.push(0x00);
        }
        Value::Date(d) => {
            buf.push(0x0A);
            buf.extend_from_slice(&((d.num_days_from_ce() as u32) ^ 0x8000_0000).to_be_bytes());
        }
        Value::Timestamp(ts) => {
            buf.push(0x0B);
            let nanos = ts.timestamp_nanos_opt().unwrap_or(0);
            buf.extend_from_slice(&((nanos as u64) ^ 0x8000_0000_0000_0000).to_be_bytes());
        }
        Value::Decimal(dec) => {
            buf.push(0x0C);
            let f = dec.to_f64().unwrap_or(0.0);
            let u = f.to_bits();
            let encoded = if (u & 0x8000_0000_0000_0000) != 0 {
                !u
            } else {
                u ^ 0x8000_0000_0000_0000
            };
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::Bytes(b) => {
            buf.push(0x0D);
            for &byte in b {
                buf.push(byte);
                if byte == 0x00 {
                    buf.push(0xFF);
                }
            }
            buf.push(0x00);
            buf.push(0x00);
        }
        Value::Uuid(u) => {
            buf.push(0x0E);
            buf.extend_from_slice(u.as_bytes());
        }
        _ => {
            buf.push(0xFF);
        }
    }
}

pub(crate) fn decode_memcomparable_value(bytes: &[u8]) -> Option<(Value, usize)> {
    if bytes.is_empty() {
        return None;
    }
    match bytes[0] {
        0x01 => Some((Value::Null, 1)),
        0x02 => {
            if bytes.len() < 2 { return None; }
            Some((Value::Boolean(bytes[1] != 0), 2))
        }
        0x03 => {
            if bytes.len() < 2 { return None; }
            let v = (bytes[1] ^ 0x80) as i8;
            Some((Value::TinyInt(v), 2))
        }
        0x04 => {
            if bytes.len() < 3 { return None; }
            let u = u16::from_be_bytes(bytes[1..3].try_into().ok()?);
            let v = (u ^ 0x8000) as i16;
            Some((Value::SmallInt(v), 3))
        }
        0x05 => {
            if bytes.len() < 5 { return None; }
            let u = u32::from_be_bytes(bytes[1..5].try_into().ok()?);
            let v = (u ^ 0x8000_0000) as i32;
            Some((Value::Integer(v), 5))
        }
        0x06 => {
            if bytes.len() < 9 { return None; }
            let u = u64::from_be_bytes(bytes[1..9].try_into().ok()?);
            let v = (u ^ 0x8000_0000_0000_0000) as i64;
            Some((Value::BigInt(v), 9))
        }
        0x07 => {
            if bytes.len() < 5 { return None; }
            let u = u32::from_be_bytes(bytes[1..5].try_into().ok()?);
            let decoded = if (u & 0x8000_0000) == 0 {
                !u
            } else {
                u ^ 0x8000_0000
            };
            Some((Value::Float(f32::from_bits(decoded)), 5))
        }
        0x08 => {
            if bytes.len() < 9 { return None; }
            let u = u64::from_be_bytes(bytes[1..9].try_into().ok()?);
            let decoded = if (u & 0x8000_0000_0000_0000) == 0 {
                !u
            } else {
                u ^ 0x8000_0000_0000_0000
            };
            Some((Value::Double(f64::from_bits(decoded)), 9))
        }
        0x09 => {
            let mut s_bytes = Vec::new();
            let mut i = 1;
            while i < bytes.len() {
                if bytes[i] == 0x00 {
                    if i + 1 < bytes.len() && bytes[i + 1] == 0xFF {
                        s_bytes.push(0x00);
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i + 1] == 0x00 {
                        i += 2;
                        break;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    s_bytes.push(bytes[i]);
                    i += 1;
                }
            }
            let s = String::from_utf8(s_bytes).ok()?;
            Some((Value::String(s), i))
        }
        0x0A => {
            if bytes.len() < 5 { return None; }
            let u = u32::from_be_bytes(bytes[1..5].try_into().ok()?);
            let num_days = (u ^ 0x8000_0000) as i32;
            let d = chrono::NaiveDate::from_num_days_from_ce_opt(num_days)?;
            Some((Value::Date(d), 5))
        }
        0x0B => {
            if bytes.len() < 9 { return None; }
            let u = u64::from_be_bytes(bytes[1..9].try_into().ok()?);
            let nanos = (u ^ 0x8000_0000_0000_0000) as i64;
            let dt = chrono::DateTime::from_timestamp_nanos(nanos);
            Some((Value::Timestamp(dt), 9))
        }
        0x0C => {
            if bytes.len() < 9 { return None; }
            let u = u64::from_be_bytes(bytes[1..9].try_into().ok()?);
            let decoded = if (u & 0x8000_0000_0000_0000) == 0 {
                !u
            } else {
                u ^ 0x8000_0000_0000_0000
            };
            let f = f64::from_bits(decoded);
            use rust_decimal::prelude::FromPrimitive;
            let dec = Decimal::from_f64(f).unwrap_or_default();
            Some((Value::Decimal(dec), 9))
        }
        0x0D => {
            let mut b_bytes = Vec::new();
            let mut i = 1;
            while i < bytes.len() {
                if bytes[i] == 0x00 {
                    if i + 1 < bytes.len() && bytes[i + 1] == 0xFF {
                        b_bytes.push(0x00);
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i + 1] == 0x00 {
                        i += 2;
                        break;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    b_bytes.push(bytes[i]);
                    i += 1;
                }
            }
            Some((Value::Bytes(b_bytes), i))
        }
        0x0E => {
            if bytes.len() < 17 { return None; }
            let u = uuid::Uuid::from_bytes(bytes[1..17].try_into().ok()?);
            Some((Value::Uuid(u), 17))
        }
        _ => None,
    }
}

pub(crate) fn decode_memcomparable_values(mut bytes: &[u8]) -> Option<Vec<Value>> {
    let mut vals = Vec::new();
    while !bytes.is_empty() {
        let (val, consumed) = decode_memcomparable_value(bytes)?;
        vals.push(val);
        bytes = &bytes[consumed..];
    }
    Some(vals)
}

pub(crate) fn encode_index_prefix(vals: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vals.len() * 9 + 1);
    for val in vals {
        encode_memcomparable_value(val, &mut bytes);
    }
    bytes.push(0x00);
    bytes
}

pub(crate) fn encode_composite_index_key(vals: &[Value], row_id: u64) -> Vec<u8> {
    let mut bytes = encode_index_prefix(vals);
    bytes.extend_from_slice(&row_id.to_be_bytes());
    bytes
}

pub(crate) fn encode_index_key(val: &Value, row_id: u64) -> Vec<u8> {
    encode_composite_index_key(std::slice::from_ref(val), row_id)
}

pub(crate) fn encode_index_key_max(vals: &[Value]) -> Vec<u8> {
    let mut bytes = encode_index_prefix(vals);
    bytes.extend_from_slice(&[0xFF; 8]);
    bytes
}

pub(crate) fn decode_composite_index_key(bytes: &[u8]) -> Option<(Vec<Value>, u64)> {
    if bytes.len() < 9 {
        return None;
    }
    let row_id_bytes = &bytes[bytes.len() - 8..];
    let row_id = u64::from_be_bytes(row_id_bytes.try_into().ok()?);
    let val_bytes = &bytes[..bytes.len() - 9];

    let vals = decode_memcomparable_values(val_bytes)?;
    Some((vals, row_id))
}

pub(crate) fn decode_index_key(bytes: &[u8]) -> Option<(Value, u64)> {
    if bytes.len() < 9 {
        return None;
    }
    let row_id_bytes = &bytes[bytes.len() - 8..];
    let row_id = u64::from_be_bytes(row_id_bytes.try_into().ok()?);
    let val_bytes = &bytes[..bytes.len() - 9];

    if let Some((v, _)) = decode_memcomparable_value(val_bytes) {
        return Some((v, row_id));
    }

    Some((Value::Null, row_id))
}

pub(crate) fn get_index_values(table_def: &TableDef, idx: &IndexDef, row: &Row) -> Option<Vec<Value>> {
    let mut vals = Vec::with_capacity(idx.columns.len());
    for col_name in &idx.columns {
        let c_idx = table_def.column_index(col_name)?;
        vals.push(row.values.get(c_idx).cloned().unwrap_or(Value::Null));
    }
    Some(vals)
}

