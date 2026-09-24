use std::collections::HashMap;

use h2_types::{DataType, H2Result, Value};

pub const SSL_REQUEST_CODE: u32 = 80877103;
pub const PROTOCOL_VERSION_3: u32 = 196608;

// PostgreSQL OID (型識別子)
pub const OID_BOOL: u32 = 16;
pub const OID_INT8: u32 = 20;
pub const OID_INT2: u32 = 21;
pub const OID_INT4: u32 = 23;
pub const OID_TEXT: u32 = 25;
pub const OID_FLOAT4: u32 = 700;
pub const OID_FLOAT8: u32 = 701;
pub const OID_NUMERIC: u32 = 1700;
pub const OID_DATE: u32 = 1082;
pub const OID_TIME: u32 = 1083;
pub const OID_TIMESTAMPTZ: u32 = 1184;
pub const OID_UUID: u32 = 2950;
pub const OID_JSON: u32 = 114;

pub fn data_type_to_oid(dt: &DataType) -> u32 {
    match dt {
        DataType::Boolean => OID_BOOL,
        DataType::TinyInt | DataType::SmallInt => OID_INT2,
        DataType::Integer => OID_INT4,
        DataType::BigInt => OID_INT8,
        DataType::Float => OID_FLOAT4,
        DataType::Double => OID_FLOAT8,
        DataType::Decimal(_, _) => OID_NUMERIC,
        DataType::Char(_) | DataType::VarChar(_) => OID_TEXT,
        DataType::Date => OID_DATE,
        DataType::Time => OID_TIME,
        DataType::Timestamp | DataType::TimestampTz => OID_TIMESTAMPTZ,
        DataType::Uuid => OID_UUID,
        DataType::Json => OID_JSON,
        _ => OID_TEXT,
    }
}

/// StartupMessage のパース結果
#[derive(Debug, Clone)]
pub struct StartupMessage {
    pub params: HashMap<String, String>,
}

impl StartupMessage {
    pub fn parse(buf: &[u8]) -> H2Result<Self> {
        let mut params = HashMap::new();
        let mut offset = 8; // length (4) + version (4)

        while offset < buf.len() {
            let key = match read_null_terminated_str(buf, offset) {
                Some((s, next)) => {
                    offset = next;
                    s
                }
                None => break,
            };

            if key.is_empty() {
                break;
            }

            let val = match read_null_terminated_str(buf, offset) {
                Some((s, next)) => {
                    offset = next;
                    s
                }
                None => break,
            };

            params.insert(key, val);
        }

        Ok(Self { params })
    }
}

fn read_null_terminated_str(buf: &[u8], start: usize) -> Option<(String, usize)> {
    let mut end = start;
    while end < buf.len() && buf[end] != 0 {
        end += 1;
    }
    if end < buf.len() {
        let s = String::from_utf8_lossy(&buf[start..end]).to_string();
        Some((s, end + 1))
    } else {
        None
    }
}

/// 送信用 PG-Wire メッセージのビルダー
pub struct PgMessageBuilder;

impl PgMessageBuilder {
    /// AuthenticationOk ('R')
    pub fn authentication_ok() -> Vec<u8> {
        let mut buf = Vec::with_capacity(9);
        buf.push(b'R');
        buf.extend_from_slice(&8u32.to_be_bytes()); // length: 4 (len) + 4 (code)
        buf.extend_from_slice(&0u32.to_be_bytes()); // 0 = AuthOK
        buf
    }

    /// ParameterStatus ('S')
    pub fn parameter_status(key: &str, value: &str) -> Vec<u8> {
        let key_bytes = key.as_bytes();
        let val_bytes = value.as_bytes();
        let len = 4 + key_bytes.len() + 1 + val_bytes.len() + 1;

        let mut buf = Vec::with_capacity(1 + len);
        buf.push(b'S');
        buf.extend_from_slice(&(len as u32).to_be_bytes());
        buf.extend_from_slice(key_bytes);
        buf.push(0);
        buf.extend_from_slice(val_bytes);
        buf.push(0);
        buf
    }

    /// ReadyForQuery ('Z')
    pub fn ready_for_query(status: u8) -> Vec<u8> {
        let mut buf = Vec::with_capacity(6);
        buf.push(b'Z');
        buf.extend_from_slice(&5u32.to_be_bytes()); // length: 4 + 1
        buf.push(status); // b'I' = Idle
        buf
    }

    /// RowDescription ('T')
    pub fn row_description(columns: &[String]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(columns.len() as u16).to_be_bytes());

        for col in columns {
            payload.extend_from_slice(col.as_bytes());
            payload.push(0); // null-terminated col name
            payload.extend_from_slice(&0u32.to_be_bytes()); // table OID
            payload.extend_from_slice(&0u16.to_be_bytes()); // column attr num
            payload.extend_from_slice(&OID_TEXT.to_be_bytes()); // data type OID (TEXT)
            payload.extend_from_slice(&(-1i16).to_be_bytes()); // data type size (-1: varlen)
            payload.extend_from_slice(&(-1i32).to_be_bytes()); // type modifier
            payload.extend_from_slice(&0u16.to_be_bytes()); // format code (0: text)
        }

        let len = 4 + payload.len() as u32;
        let mut buf = Vec::with_capacity(1 + payload.len() + 4);
        buf.push(b'T');
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&payload);
        buf
    }

    /// DataRow ('D')
    pub fn data_row(values: &[Value]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(values.len() as u16).to_be_bytes());

        for val in values {
            if val.is_null() {
                payload.extend_from_slice(&(-1i32).to_be_bytes());
            } else {
                let s = match val {
                    Value::String(str_val) => str_val.clone(),
                    other => other.to_string(),
                };
                let bytes = s.as_bytes();
                payload.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                payload.extend_from_slice(bytes);
            }
        }

        let len = 4 + payload.len() as u32;
        let mut buf = Vec::with_capacity(1 + payload.len() + 4);
        buf.push(b'D');
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&payload);
        buf
    }

    /// CommandComplete ('C')
    pub fn command_complete(tag: &str) -> Vec<u8> {
        let tag_bytes = tag.as_bytes();
        let len = 4 + tag_bytes.len() + 1;

        let mut buf = Vec::with_capacity(1 + len);
        buf.push(b'C');
        buf.extend_from_slice(&(len as u32).to_be_bytes());
        buf.extend_from_slice(tag_bytes);
        buf.push(0);
        buf
    }

    /// ErrorResponse ('E')
    pub fn error_response(msg: &str) -> Vec<u8> {
        let mut payload = Vec::new();
        // 'S': Severity
        payload.push(b'S');
        payload.extend_from_slice(b"ERROR\0");
        // 'C': SQLSTATE
        payload.push(b'C');
        payload.extend_from_slice(b"XX000\0");
        // 'M': Message
        payload.push(b'M');
        payload.extend_from_slice(msg.as_bytes());
        payload.push(0);
        payload.push(0); // 終端

        let len = 4 + payload.len() as u32;
        let mut buf = Vec::with_capacity(1 + payload.len() + 4);
        buf.push(b'E');
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&payload);
        buf
    }
}
