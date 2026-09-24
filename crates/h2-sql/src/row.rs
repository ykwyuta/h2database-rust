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
        serde_json::to_vec(self).map_err(|e| H2Error::Serialization(e.to_string()))
    }

    pub fn from_bytes(bytes: &[u8]) -> H2Result<Self> {
        serde_json::from_slice(bytes).map_err(|e| H2Error::Serialization(e.to_string()))
    }
}
