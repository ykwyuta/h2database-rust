use std::collections::HashMap;
use std::fmt;
use serde::{Deserialize, Serialize};
use h2_types::Value;

/// グラフノード（頂点: Vertex / Node）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    pub labels: Vec<String>,
    pub properties: HashMap<String, Value>,
}

impl Node {
    pub fn new(id: u64, labels: Vec<String>, properties: HashMap<String, Value>) -> Self {
        Self {
            id,
            labels,
            properties,
        }
    }

    pub fn has_label(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l.eq_ignore_ascii_case(label))
    }

    pub fn get_property(&self, key: &str) -> Option<&Value> {
        self.properties.get(key)
    }
}

/// グラフエッジ（関係性: Edge / Relationship）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: u64,
    pub edge_type: String,
    pub src_id: u64,
    pub dst_id: u64,
    pub properties: HashMap<String, Value>,
}

impl Edge {
    pub fn new(
        id: u64,
        edge_type: impl Into<String>,
        src_id: u64,
        dst_id: u64,
        properties: HashMap<String, Value>,
    ) -> Self {
        Self {
            id,
            edge_type: edge_type.into(),
            src_id,
            dst_id,
            properties,
        }
    }

    pub fn get_property(&self, key: &str) -> Option<&Value> {
        self.properties.get(key)
    }
}

/// パス（連続するノードとエッジの連鎖）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Path {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Path {
    pub fn new(nodes: Vec<Node>, edges: Vec<Edge>) -> Self {
        Self { nodes, edges }
    }

    pub fn len(&self) -> usize {
        self.edges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

/// Cypher クエリの評価値（スカラー値、ノード、エッジ、パス、コレクション等）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GraphValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Node(Node),
    Edge(Edge),
    Path(Path),
    List(Vec<GraphValue>),
    Map(HashMap<String, GraphValue>),
}

impl GraphValue {
    pub fn is_null(&self) -> bool {
        matches!(self, GraphValue::Null)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            GraphValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            GraphValue::Integer(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            GraphValue::Float(f) => Some(*f),
            GraphValue::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            GraphValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn from_value(val: &Value) -> Self {
        match val {
            Value::Null => GraphValue::Null,
            Value::Boolean(b) => GraphValue::Boolean(*b),
            Value::TinyInt(i) => GraphValue::Integer(*i as i64),
            Value::SmallInt(i) => GraphValue::Integer(*i as i64),
            Value::Integer(i) => GraphValue::Integer(*i as i64),
            Value::BigInt(i) => GraphValue::Integer(*i),
            Value::Float(f) => GraphValue::Float(*f as f64),
            Value::Double(d) => GraphValue::Float(*d),
            Value::String(s) => GraphValue::String(s.clone()),
            Value::Decimal(d) => GraphValue::String(d.to_string()),
            Value::Date(d) => GraphValue::String(d.to_string()),
            Value::Time(t) => GraphValue::String(t.to_string()),
            Value::Timestamp(t) => GraphValue::String(t.to_string()),
            Value::Bytes(b) => GraphValue::String(format!("<bytes len={}>", b.len())),
            Value::Json(j) => GraphValue::String(j.to_string()),
            Value::Uuid(u) => GraphValue::String(u.to_string()),
            Value::Array(arr) => GraphValue::List(arr.iter().map(GraphValue::from_value).collect()),
            Value::Interval(iv) => GraphValue::String(format!("{iv:?}")),
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            GraphValue::Null => Value::Null,
            GraphValue::Boolean(b) => Value::Boolean(*b),
            GraphValue::Integer(i) => Value::BigInt(*i),
            GraphValue::Float(f) => Value::Double(*f),
            GraphValue::String(s) => Value::String(s.clone()),
            GraphValue::Node(n) => Value::String(format!("(:{} {{{}}})", n.labels.join(":"), n.id)),
            GraphValue::Edge(e) => Value::String(format!("[:{} {{{}}}]", e.edge_type, e.id)),
            GraphValue::Path(p) => Value::String(format!("<path len={}>", p.len())),
            GraphValue::List(items) => {
                let json_items: Vec<serde_json::Value> = items.iter().map(|item| serde_json::to_value(item).unwrap_or(serde_json::Value::Null)).collect();
                Value::Json(serde_json::Value::Array(json_items))
            }
            GraphValue::Map(m) => {
                let json_map: serde_json::Map<String, serde_json::Value> = m.iter().map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(serde_json::Value::Null))).collect();
                Value::Json(serde_json::Value::Object(json_map))
            }
        }
    }
}

impl fmt::Display for GraphValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphValue::Null => write!(f, "null"),
            GraphValue::Boolean(b) => write!(f, "{}", b),
            GraphValue::Integer(i) => write!(f, "{}", i),
            GraphValue::Float(fl) => write!(f, "{}", fl),
            GraphValue::String(s) => write!(f, "\"{}\"", s),
            GraphValue::Node(n) => write!(f, "(:{} {{{}}})", n.labels.join(":"), n.id),
            GraphValue::Edge(e) => write!(f, "-[:{} {{{}}}]->", e.edge_type, e.id),
            GraphValue::Path(p) => write!(f, "<path len={}>", p.len()),
            GraphValue::List(l) => {
                write!(f, "[")?;
                for (i, v) in l.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            GraphValue::Map(m) => {
                write!(f, "{{")?;
                let mut first = true;
                for (k, v) in m {
                    if !first { write!(f, ", ")?; }
                    first = false;
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, "}}")
            }
        }
    }
}

/// グラフ更新統計
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphStats {
    pub nodes_created: usize,
    pub nodes_deleted: usize,
    pub relationships_created: usize,
    pub relationships_deleted: usize,
    pub properties_set: usize,
    pub labels_added: usize,
}

/// グラフクエリ実行結果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<GraphValue>>,
    pub stats: GraphStats,
}

impl GraphResult {
    pub fn empty() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            stats: GraphStats::default(),
        }
    }

    pub fn with_columns_and_rows(columns: Vec<String>, rows: Vec<Vec<GraphValue>>) -> Self {
        Self {
            columns,
            rows,
            stats: GraphStats::default(),
        }
    }
}
