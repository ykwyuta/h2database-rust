use std::collections::HashMap;
use h2_graph::{Edge, GraphValue, Node, Path};
use h2_types::{H2Error, H2Result};

/// PackStream 値表現
#[derive(Debug, Clone, PartialEq)]
pub enum PackValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<PackValue>),
    Map(HashMap<String, PackValue>),
    Structure {
        tag: u8,
        fields: Vec<PackValue>,
    },
}

impl PackValue {
    pub fn from_graph_value(gv: &GraphValue) -> Self {
        match gv {
            GraphValue::Null => PackValue::Null,
            GraphValue::Boolean(b) => PackValue::Boolean(*b),
            GraphValue::Integer(i) => PackValue::Integer(*i),
            GraphValue::Float(f) => PackValue::Float(*f),
            GraphValue::String(s) => PackValue::String(s.clone()),
            GraphValue::Node(node) => Self::from_node(node),
            GraphValue::Edge(edge) => Self::from_edge(edge),
            GraphValue::Path(path) => Self::from_path(path),
            GraphValue::List(list) => {
                PackValue::List(list.iter().map(Self::from_graph_value).collect())
            }
            GraphValue::Map(map) => {
                let mut pmap = HashMap::new();
                for (k, v) in map {
                    pmap.insert(k.clone(), Self::from_graph_value(v));
                }
                PackValue::Map(pmap)
            }
        }
    }

    pub fn to_graph_value(&self) -> GraphValue {
        match self {
            PackValue::Null => GraphValue::Null,
            PackValue::Boolean(b) => GraphValue::Boolean(*b),
            PackValue::Integer(i) => GraphValue::Integer(*i),
            PackValue::Float(f) => GraphValue::Float(*f),
            PackValue::String(s) => GraphValue::String(s.clone()),
            PackValue::Bytes(b) => GraphValue::String(format!("<bytes len={}>", b.len())),
            PackValue::List(list) => {
                GraphValue::List(list.iter().map(|item| item.to_graph_value()).collect())
            }
            PackValue::Map(map) => {
                let mut gmap = HashMap::new();
                for (k, v) in map {
                    gmap.insert(k.clone(), v.to_graph_value());
                }
                GraphValue::Map(gmap)
            }
            PackValue::Structure { tag, fields } => {
                // If tag is Node (0x4E)
                if *tag == 0x4E && fields.len() >= 3 {
                    let id = if let PackValue::Integer(id) = fields[0] {
                        id as u64
                    } else {
                        0
                    };
                    let labels = if let PackValue::List(ref l) = fields[1] {
                        l.iter()
                            .filter_map(|pv| match pv {
                                PackValue::String(s) => Some(s.clone()),
                                _ => None,
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let mut properties = HashMap::new();
                    if let PackValue::Map(ref m) = fields[2] {
                        for (k, v) in m {
                            properties.insert(k.clone(), v.to_graph_value().to_value());
                        }
                    }
                    GraphValue::Node(Node::new(id, labels, properties))
                } else if *tag == 0x52 && fields.len() >= 5 {
                    // Relationship (0x52)
                    let id = if let PackValue::Integer(id) = fields[0] {
                        id as u64
                    } else {
                        0
                    };
                    let src_id = if let PackValue::Integer(id) = fields[1] {
                        id as u64
                    } else {
                        0
                    };
                    let dst_id = if let PackValue::Integer(id) = fields[2] {
                        id as u64
                    } else {
                        0
                    };
                    let edge_type = if let PackValue::String(ref s) = fields[3] {
                        s.clone()
                    } else {
                        "RELATED".to_string()
                    };
                    let mut properties = HashMap::new();
                    if let PackValue::Map(ref m) = fields[4] {
                        for (k, v) in m {
                            properties.insert(k.clone(), v.to_graph_value().to_value());
                        }
                    }
                    GraphValue::Edge(Edge::new(id, edge_type, src_id, dst_id, properties))
                } else {
                    GraphValue::String(format!("<struct tag={tag:#x}>"))
                }
            }
        }
    }

    fn from_node(node: &Node) -> Self {
        // Bolt Node Structure: tag 0x4E ('N'), fields: [id: int, labels: [str], properties: {k: v}]
        let labels = PackValue::List(
            node.labels
                .iter()
                .map(|l| PackValue::String(l.clone()))
                .collect(),
        );
        let mut props = HashMap::new();
        for (k, v) in &node.properties {
            props.insert(
                k.clone(),
                PackValue::from_graph_value(&GraphValue::from_value(v)),
            );
        }
        PackValue::Structure {
            tag: 0x4E,
            fields: vec![
                PackValue::Integer(node.id as i64),
                labels,
                PackValue::Map(props),
            ],
        }
    }

    fn from_edge(edge: &Edge) -> Self {
        // Bolt Relationship Structure: tag 0x52 ('R'), fields: [id: int, startNodeId: int, endNodeId: int, type: str, properties: {k: v}]
        let mut props = HashMap::new();
        for (k, v) in &edge.properties {
            props.insert(
                k.clone(),
                PackValue::from_graph_value(&GraphValue::from_value(v)),
            );
        }
        PackValue::Structure {
            tag: 0x52,
            fields: vec![
                PackValue::Integer(edge.id as i64),
                PackValue::Integer(edge.src_id as i64),
                PackValue::Integer(edge.dst_id as i64),
                PackValue::String(edge.edge_type.clone()),
                PackValue::Map(props),
            ],
        }
    }

    fn from_path(path: &Path) -> Self {
        // Bolt Path Structure: tag 0x50 ('P'), fields: [nodes: [Node], rels: [UnboundRel], sequence: [int]]
        let nodes = PackValue::List(path.nodes.iter().map(Self::from_node).collect());

        let mut rels = Vec::new();
        let mut sequence = Vec::new();

        for (i, edge) in path.edges.iter().enumerate() {
            let mut props = HashMap::new();
            for (k, v) in &edge.properties {
                props.insert(
                    k.clone(),
                    PackValue::from_graph_value(&GraphValue::from_value(v)),
                );
            }
            // UnboundRelationship: tag 0x72 ('r'), fields: [id: int, type: str, properties: {k: v}]
            let unbound_rel = PackValue::Structure {
                tag: 0x72,
                fields: vec![
                    PackValue::Integer(edge.id as i64),
                    PackValue::String(edge.edge_type.clone()),
                    PackValue::Map(props),
                ],
            };
            rels.push(unbound_rel);
            // Sequence indices: edge index (1-based signed: positive = outgoing, negative = incoming), node index
            sequence.push(PackValue::Integer((i + 1) as i64));
            sequence.push(PackValue::Integer((i + 1) as i64));
        }

        PackValue::Structure {
            tag: 0x50,
            fields: vec![
                nodes,
                PackValue::List(rels),
                PackValue::List(sequence),
            ],
        }
    }

    // ================= PackStream シリアライザ =================

    pub fn encode(&self, buf: &mut Vec<u8>) {
        match self {
            PackValue::Null => buf.push(0xC0),
            PackValue::Boolean(false) => buf.push(0xC2),
            PackValue::Boolean(true) => buf.push(0xC3),
            PackValue::Integer(i) => Self::encode_integer(*i, buf),
            PackValue::Float(f) => {
                buf.push(0xC1);
                buf.extend_from_slice(&f.to_be_bytes());
            }
            PackValue::String(s) => Self::encode_string(s, buf),
            PackValue::Bytes(b) => Self::encode_bytes(b, buf),
            PackValue::List(items) => Self::encode_list(items, buf),
            PackValue::Map(map) => Self::encode_map(map, buf),
            PackValue::Structure { tag, fields } => Self::encode_struct(*tag, fields, buf),
        }
    }

    fn encode_integer(i: i64, buf: &mut Vec<u8>) {
        if (-16..=127).contains(&i) {
            buf.push((i as i8) as u8);
        } else if (-128..=127).contains(&i) {
            buf.push(0xC8);
            buf.push((i as i8) as u8);
        } else if (-32768..=32767).contains(&i) {
            buf.push(0xC9);
            buf.extend_from_slice(&(i as i16).to_be_bytes());
        } else if (-2147483648..=2147483647).contains(&i) {
            buf.push(0xCA);
            buf.extend_from_slice(&(i as i32).to_be_bytes());
        } else {
            buf.push(0xCB);
            buf.extend_from_slice(&i.to_be_bytes());
        }
    }

    fn encode_string(s: &str, buf: &mut Vec<u8>) {
        let len = s.len();
        if len <= 15 {
            buf.push(0x80 | (len as u8));
        } else if len <= 255 {
            buf.push(0xD0);
            buf.push(len as u8);
        } else if len <= 65535 {
            buf.push(0xD1);
            buf.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            buf.push(0xD2);
            buf.extend_from_slice(&(len as u32).to_be_bytes());
        }
        buf.extend_from_slice(s.as_bytes());
    }

    fn encode_bytes(b: &[u8], buf: &mut Vec<u8>) {
        let len = b.len();
        if len <= 255 {
            buf.push(0xCC);
            buf.push(len as u8);
        } else if len <= 65535 {
            buf.push(0xCD);
            buf.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            buf.push(0xCE);
            buf.extend_from_slice(&(len as u32).to_be_bytes());
        }
        buf.extend_from_slice(b);
    }

    fn encode_list(items: &[PackValue], buf: &mut Vec<u8>) {
        let len = items.len();
        if len <= 15 {
            buf.push(0x90 | (len as u8));
        } else if len <= 255 {
            buf.push(0xD4);
            buf.push(len as u8);
        } else if len <= 65535 {
            buf.push(0xD5);
            buf.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            buf.push(0xD6);
            buf.extend_from_slice(&(len as u32).to_be_bytes());
        }
        for item in items {
            item.encode(buf);
        }
    }

    fn encode_map(map: &HashMap<String, PackValue>, buf: &mut Vec<u8>) {
        let len = map.len();
        if len <= 15 {
            buf.push(0xA0 | (len as u8));
        } else if len <= 255 {
            buf.push(0xD8);
            buf.push(len as u8);
        } else if len <= 65535 {
            buf.push(0xD9);
            buf.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            buf.push(0xDA);
            buf.extend_from_slice(&(len as u32).to_be_bytes());
        }
        for (k, v) in map {
            Self::encode_string(k, buf);
            v.encode(buf);
        }
    }

    fn encode_struct(tag: u8, fields: &[PackValue], buf: &mut Vec<u8>) {
        let len = fields.len();
        if len <= 15 {
            buf.push(0xB0 | (len as u8));
        } else if len <= 255 {
            buf.push(0xDC);
            buf.push(len as u8);
        } else {
            buf.push(0xDD);
            buf.extend_from_slice(&(len as u16).to_be_bytes());
        }
        buf.push(tag);
        for f in fields {
            f.encode(buf);
        }
    }

    // ================= PackStream デシリアライザ =================

    pub fn decode(buf: &[u8]) -> H2Result<(Self, usize)> {
        if buf.is_empty() {
            return Err(H2Error::Execution("Empty buffer in PackStream decode".to_string()));
        }

        let marker = buf[0];
        match marker {
            // Null
            0xC0 => Ok((PackValue::Null, 1)),

            // Boolean
            0xC2 => Ok((PackValue::Boolean(false), 1)),
            0xC3 => Ok((PackValue::Boolean(true), 1)),

            // Float
            0xC1 => {
                if buf.len() < 9 {
                    return Err(H2Error::Execution("Truncated float".to_string()));
                }
                let bytes: [u8; 8] = buf[1..9].try_into().unwrap();
                Ok((PackValue::Float(f64::from_be_bytes(bytes)), 9))
            }

            // TinyInt (positive: 0..127)
            0x00..=0x7F => Ok((PackValue::Integer(marker as i64), 1)),

            // TinyInt (negative: -16..-1)
            0xF0..=0xFF => Ok((PackValue::Integer((marker as i8) as i64), 1)),

            // Int8
            0xC8 => {
                if buf.len() < 2 {
                    return Err(H2Error::Execution("Truncated Int8".to_string()));
                }
                Ok((PackValue::Integer((buf[1] as i8) as i64), 2))
            }

            // Int16
            0xC9 => {
                if buf.len() < 3 {
                    return Err(H2Error::Execution("Truncated Int16".to_string()));
                }
                let i = i16::from_be_bytes(buf[1..3].try_into().unwrap());
                Ok((PackValue::Integer(i as i64), 3))
            }

            // Int32
            0xCA => {
                if buf.len() < 5 {
                    return Err(H2Error::Execution("Truncated Int32".to_string()));
                }
                let i = i32::from_be_bytes(buf[1..5].try_into().unwrap());
                Ok((PackValue::Integer(i as i64), 5))
            }

            // Int64
            0xCB => {
                if buf.len() < 9 {
                    return Err(H2Error::Execution("Truncated Int64".to_string()));
                }
                let i = i64::from_be_bytes(buf[1..9].try_into().unwrap());
                Ok((PackValue::Integer(i), 9))
            }

            // TinyString (0x80..=0x8F)
            0x80..=0x8F => {
                let len = (marker & 0x0F) as usize;
                Self::decode_string_content(&buf[1..], len, 1)
            }

            // String8
            0xD0 => {
                if buf.len() < 2 {
                    return Err(H2Error::Execution("Truncated String8".to_string()));
                }
                let len = buf[1] as usize;
                Self::decode_string_content(&buf[2..], len, 2)
            }

            // String16
            0xD1 => {
                if buf.len() < 3 {
                    return Err(H2Error::Execution("Truncated String16".to_string()));
                }
                let len = u16::from_be_bytes(buf[1..3].try_into().unwrap()) as usize;
                Self::decode_string_content(&buf[3..], len, 3)
            }

            // String32
            0xD2 => {
                if buf.len() < 5 {
                    return Err(H2Error::Execution("Truncated String32".to_string()));
                }
                let len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
                Self::decode_string_content(&buf[5..], len, 5)
            }

            // TinyList (0x90..=0x9F)
            0x90..=0x9F => {
                let count = (marker & 0x0F) as usize;
                Self::decode_list_content(&buf[1..], count, 1)
            }

            // List8
            0xD4 => {
                if buf.len() < 2 {
                    return Err(H2Error::Execution("Truncated List8".to_string()));
                }
                let count = buf[1] as usize;
                Self::decode_list_content(&buf[2..], count, 2)
            }

            // List16
            0xD5 => {
                if buf.len() < 3 {
                    return Err(H2Error::Execution("Truncated List16".to_string()));
                }
                let count = u16::from_be_bytes(buf[1..3].try_into().unwrap()) as usize;
                Self::decode_list_content(&buf[3..], count, 3)
            }

            // List32
            0xD6 => {
                if buf.len() < 5 {
                    return Err(H2Error::Execution("Truncated List32".to_string()));
                }
                let count = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
                Self::decode_list_content(&buf[5..], count, 5)
            }

            // TinyMap (0xA0..=0xAF)
            0xA0..=0xAF => {
                let count = (marker & 0x0F) as usize;
                Self::decode_map_content(&buf[1..], count, 1)
            }

            // Map8
            0xD8 => {
                if buf.len() < 2 {
                    return Err(H2Error::Execution("Truncated Map8".to_string()));
                }
                let count = buf[1] as usize;
                Self::decode_map_content(&buf[2..], count, 2)
            }

            // Map16
            0xD9 => {
                if buf.len() < 3 {
                    return Err(H2Error::Execution("Truncated Map16".to_string()));
                }
                let count = u16::from_be_bytes(buf[1..3].try_into().unwrap()) as usize;
                Self::decode_map_content(&buf[3..], count, 3)
            }

            // Map32
            0xDA => {
                if buf.len() < 5 {
                    return Err(H2Error::Execution("Truncated Map32".to_string()));
                }
                let count = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
                Self::decode_map_content(&buf[5..], count, 5)
            }

            // TinyStruct (0xB0..=0xBF)
            0xB0..=0xBF => {
                let count = (marker & 0x0F) as usize;
                Self::decode_struct_content(&buf[1..], count, 1)
            }

            // Struct8
            0xDC => {
                if buf.len() < 2 {
                    return Err(H2Error::Execution("Truncated Struct8".to_string()));
                }
                let count = buf[1] as usize;
                Self::decode_struct_content(&buf[2..], count, 2)
            }

            // Struct16
            0xDD => {
                if buf.len() < 3 {
                    return Err(H2Error::Execution("Truncated Struct16".to_string()));
                }
                let count = u16::from_be_bytes(buf[1..3].try_into().unwrap()) as usize;
                Self::decode_struct_content(&buf[3..], count, 3)
            }

            other => Err(H2Error::Execution(format!(
                "Unknown PackStream marker: {other:#x}"
            ))),
        }
    }

    fn decode_string_content(
        buf: &[u8],
        len: usize,
        header_len: usize,
    ) -> H2Result<(PackValue, usize)> {
        if buf.len() < len {
            return Err(H2Error::Execution("Truncated string data".to_string()));
        }
        let s = String::from_utf8(buf[..len].to_vec())
            .map_err(|e| H2Error::Execution(format!("Invalid UTF-8 string: {e}")))?;
        Ok((PackValue::String(s), header_len + len))
    }

    fn decode_list_content(
        buf: &[u8],
        count: usize,
        header_len: usize,
    ) -> H2Result<(PackValue, usize)> {
        let mut list = Vec::with_capacity(count);
        let mut offset = 0;
        for _ in 0..count {
            let (item, consumed) = Self::decode(&buf[offset..])?;
            list.push(item);
            offset += consumed;
        }
        Ok((PackValue::List(list), header_len + offset))
    }

    fn decode_map_content(
        buf: &[u8],
        count: usize,
        header_len: usize,
    ) -> H2Result<(PackValue, usize)> {
        let mut map = HashMap::with_capacity(count);
        let mut offset = 0;
        for _ in 0..count {
            let (key_val, k_consumed) = Self::decode(&buf[offset..])?;
            offset += k_consumed;
            let key = match key_val {
                PackValue::String(s) => s,
                _ => return Err(H2Error::Execution("Map key must be String".to_string())),
            };

            let (val, v_consumed) = Self::decode(&buf[offset..])?;
            offset += v_consumed;
            map.insert(key, val);
        }
        Ok((PackValue::Map(map), header_len + offset))
    }

    fn decode_struct_content(
        buf: &[u8],
        count: usize,
        header_len: usize,
    ) -> H2Result<(PackValue, usize)> {
        if buf.is_empty() {
            return Err(H2Error::Execution("Truncated structure tag".to_string()));
        }
        let tag = buf[0];
        let mut fields = Vec::with_capacity(count);
        let mut offset = 1;

        for _ in 0..count {
            let (field, consumed) = Self::decode(&buf[offset..])?;
            fields.push(field);
            offset += consumed;
        }

        Ok((PackValue::Structure { tag, fields }, header_len + offset))
    }
}
