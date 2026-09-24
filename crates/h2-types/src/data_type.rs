use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataType {
    // プリミティブ型
    Boolean,
    TinyInt,           // i8
    SmallInt,          // i16
    Integer,           // i32
    BigInt,            // i64
    Float,             // f32
    Double,            // f64
    Decimal(u8, u8),   // precision, scale

    // 文字列・バイナリ
    Char(usize),
    VarChar(Option<usize>),
    Binary(Option<usize>),
    Blob,

    // 日時型
    Date,
    Time,
    Timestamp,
    TimestampTz,

    // モダン拡張型
    Uuid,
    Json,
    Array(Box<DataType>),
    Vector(usize), // 次元数
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataType::Boolean => write!(f, "BOOLEAN"),
            DataType::TinyInt => write!(f, "TINYINT"),
            DataType::SmallInt => write!(f, "SMALLINT"),
            DataType::Integer => write!(f, "INTEGER"),
            DataType::BigInt => write!(f, "BIGINT"),
            DataType::Float => write!(f, "FLOAT"),
            DataType::Double => write!(f, "DOUBLE PRECISION"),
            DataType::Decimal(p, s) => write!(f, "DECIMAL({}, {})", p, s),
            DataType::Char(len) => write!(f, "CHAR({})", len),
            DataType::VarChar(Some(len)) => write!(f, "VARCHAR({})", len),
            DataType::VarChar(None) => write!(f, "VARCHAR"),
            DataType::Binary(Some(len)) => write!(f, "BINARY({})", len),
            DataType::Binary(None) => write!(f, "VARBINARY"),
            DataType::Blob => write!(f, "BLOB"),
            DataType::Date => write!(f, "DATE"),
            DataType::Time => write!(f, "TIME"),
            DataType::Timestamp => write!(f, "TIMESTAMP"),
            DataType::TimestampTz => write!(f, "TIMESTAMP WITH TIME ZONE"),
            DataType::Uuid => write!(f, "UUID"),
            DataType::Json => write!(f, "JSON"),
            DataType::Array(inner) => write!(f, "{}[]", inner),
            DataType::Vector(dim) => write!(f, "VECTOR({})", dim),
        }
    }
}
