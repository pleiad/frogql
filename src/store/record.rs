/// Binary encoding for node and edge records stored in page cells.
///
/// Node cell format:
/// ```text
/// [user_id_str_id: u32 LE]      — string table ID of the user-facing ID ("a1", "n1")
/// [label_count: u16 LE]
/// [label_str_ids: u32 LE × label_count]
/// [prop_count: u16 LE]
/// [properties...]
///   each: [name_str_id: u32 LE][value_type: u8][value_data: variable]
///     value_type 0 = Int:    [i64 LE, 8 bytes]
///     value_type 1 = Str:    [str_id: u32 LE]
///     value_type 2 = Bool:   [u8: 0 or 1]
///     value_type 3 = Float:  [f64 LE, 8 bytes]
///     value_type 4 = List:   [length: u32 LE][element] × length
///     value_type 5 = Record: [field_count: u32 LE][name_str_id: u32 LE, value] × field_count
///     value_type 6 = Null:   no payload (used inside lists / records;
///                            top-level null is encoded as key absence)
/// ```
///
/// Edge cell format = Node cell format + :
/// ```text
/// [src_internal_id: u32 LE]    — internal sequential ID of source node
/// [tgt_internal_id: u32 LE]    — internal sequential ID of target node
/// [directionality: u8]         — 0 = directed (->), 1 = undirected (~~)
/// ```
pub const VALUE_TYPE_INT: u8 = 0;
pub const VALUE_TYPE_STR: u8 = 1;
pub const VALUE_TYPE_BOOL: u8 = 2;
pub const VALUE_TYPE_FLOAT: u8 = 3;
pub const VALUE_TYPE_LIST: u8 = 4;
pub const VALUE_TYPE_RECORD: u8 = 5;
pub const VALUE_TYPE_NULL: u8 = 6;

pub const DIR_DIRECTED: u8 = 0;
pub const DIR_UNDIRECTED: u8 = 1;

/// Encode a node record into bytes.
pub fn encode_node(
    user_id_str_id: u32,
    label_str_ids: &[u32],
    props: &[(u32, PropValue)], // (name_str_id, value)
) -> Vec<u8> {
    let mut buf = Vec::new();

    buf.extend_from_slice(&user_id_str_id.to_le_bytes());

    buf.extend_from_slice(&(label_str_ids.len() as u16).to_le_bytes());
    for &lid in label_str_ids {
        buf.extend_from_slice(&lid.to_le_bytes());
    }

    buf.extend_from_slice(&(props.len() as u16).to_le_bytes());
    for (name_sid, val) in props {
        buf.extend_from_slice(&name_sid.to_le_bytes());
        encode_prop_value(&mut buf, val);
    }

    buf
}

/// Encode an edge record into bytes (node record + endpoint info).
pub fn encode_edge(
    user_id_str_id: u32,
    label_str_ids: &[u32],
    props: &[(u32, PropValue)],
    src_internal_id: u32,
    tgt_internal_id: u32,
    directed: bool,
) -> Vec<u8> {
    let mut buf = encode_node(user_id_str_id, label_str_ids, props);
    buf.extend_from_slice(&src_internal_id.to_le_bytes());
    buf.extend_from_slice(&tgt_internal_id.to_le_bytes());
    buf.push(if directed {
        DIR_DIRECTED
    } else {
        DIR_UNDIRECTED
    });
    buf
}

/// A property value in its encoded form.
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    Int(i64),
    Str(u32), // string table ID
    Bool(bool),
    Float(f64),
    List(Vec<PropValue>),
    Record(Vec<(u32, PropValue)>), // (name_str_id, value); sorted by key for deterministic encoding
    /// SQL-style null. Top-level nulls are encoded by omitting the
    /// property entirely; this variant only appears inside `List` /
    /// `Record` payloads where positional alignment forces an explicit
    /// marker.
    Null,
}

fn encode_prop_value(buf: &mut Vec<u8>, val: &PropValue) {
    match val {
        PropValue::Null => {
            buf.push(VALUE_TYPE_NULL);
        }
        PropValue::Int(n) => {
            buf.push(VALUE_TYPE_INT);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        PropValue::Str(sid) => {
            buf.push(VALUE_TYPE_STR);
            buf.extend_from_slice(&sid.to_le_bytes());
        }
        PropValue::Bool(b) => {
            buf.push(VALUE_TYPE_BOOL);
            buf.push(if *b { 1 } else { 0 });
        }
        PropValue::Float(x) => {
            buf.push(VALUE_TYPE_FLOAT);
            buf.extend_from_slice(&x.to_le_bytes());
        }
        PropValue::List(items) => {
            buf.push(VALUE_TYPE_LIST);
            buf.extend_from_slice(&(items.len() as u32).to_le_bytes());
            for it in items {
                encode_prop_value(buf, it);
            }
        }
        PropValue::Record(fields) => {
            buf.push(VALUE_TYPE_RECORD);
            buf.extend_from_slice(&(fields.len() as u32).to_le_bytes());
            for (name_sid, val) in fields {
                buf.extend_from_slice(&name_sid.to_le_bytes());
                encode_prop_value(buf, val);
            }
        }
    }
}

/// Decoded node record.
#[derive(Debug)]
pub struct DecodedNode {
    pub user_id_str_id: u32,
    pub label_str_ids: Vec<u32>,
    pub props: Vec<(u32, PropValue)>, // (name_str_id, value)
}

/// Decoded edge record.
#[derive(Debug)]
pub struct DecodedEdge {
    pub node: DecodedNode, // shared fields
    pub src_internal_id: u32,
    pub tgt_internal_id: u32,
    pub directed: bool,
}

/// Decode a node record from bytes. Returns (decoded_node, bytes_consumed).
pub fn decode_node(data: &[u8]) -> (DecodedNode, usize) {
    let mut pos = 0;

    let user_id_str_id = read_u32(data, &mut pos);
    let label_count = read_u16(data, &mut pos) as usize;
    let mut label_str_ids = Vec::with_capacity(label_count);
    for _ in 0..label_count {
        label_str_ids.push(read_u32(data, &mut pos));
    }

    let prop_count = read_u16(data, &mut pos) as usize;
    let mut props = Vec::with_capacity(prop_count);
    for _ in 0..prop_count {
        let name_sid = read_u32(data, &mut pos);
        let val = decode_prop_value(data, &mut pos);
        props.push((name_sid, val));
    }

    (
        DecodedNode {
            user_id_str_id,
            label_str_ids,
            props,
        },
        pos,
    )
}

/// Decode an edge record from bytes.
pub fn decode_edge(data: &[u8]) -> DecodedEdge {
    let (node, mut pos) = decode_node(data);
    let src_internal_id = read_u32(data, &mut pos);
    let tgt_internal_id = read_u32(data, &mut pos);
    let directed = data[pos] == DIR_DIRECTED;

    DecodedEdge {
        node,
        src_internal_id,
        tgt_internal_id,
        directed,
    }
}

fn decode_prop_value(data: &[u8], pos: &mut usize) -> PropValue {
    let vtype = data[*pos];
    *pos += 1;
    match vtype {
        VALUE_TYPE_NULL => PropValue::Null,
        VALUE_TYPE_INT => {
            let n = i64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap());
            *pos += 8;
            PropValue::Int(n)
        }
        VALUE_TYPE_STR => {
            let sid = u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap());
            *pos += 4;
            PropValue::Str(sid)
        }
        VALUE_TYPE_BOOL => {
            let b = data[*pos] != 0;
            *pos += 1;
            PropValue::Bool(b)
        }
        VALUE_TYPE_FLOAT => {
            let x = f64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap());
            *pos += 8;
            PropValue::Float(x)
        }
        VALUE_TYPE_LIST => {
            let len = u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap()) as usize;
            *pos += 4;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(decode_prop_value(data, pos));
            }
            PropValue::List(items)
        }
        VALUE_TYPE_RECORD => {
            let len = u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap()) as usize;
            *pos += 4;
            let mut fields = Vec::with_capacity(len);
            for _ in 0..len {
                let name_sid = read_u32(data, pos);
                let v = decode_prop_value(data, pos);
                fields.push((name_sid, v));
            }
            PropValue::Record(fields)
        }
        _ => panic!("unknown property value type: {vtype}"),
    }
}

fn read_u32(data: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    v
}

fn read_u16(data: &[u8], pos: &mut usize) -> u16 {
    let v = u16::from_le_bytes(data[*pos..*pos + 2].try_into().unwrap());
    *pos += 2;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_roundtrip() {
        let props = vec![
            (10, PropValue::Str(20)),
            (11, PropValue::Bool(true)),
            (12, PropValue::Int(42)),
        ];
        let encoded = encode_node(5, &[1, 2, 3], &props);
        let (decoded, len) = decode_node(&encoded);

        assert_eq!(len, encoded.len());
        assert_eq!(decoded.user_id_str_id, 5);
        assert_eq!(decoded.label_str_ids, vec![1, 2, 3]);
        assert_eq!(decoded.props.len(), 3);
        assert_eq!(decoded.props[0], (10, PropValue::Str(20)));
        assert_eq!(decoded.props[1], (11, PropValue::Bool(true)));
        assert_eq!(decoded.props[2], (12, PropValue::Int(42)));
    }

    #[test]
    fn test_edge_roundtrip() {
        let props = vec![(7, PropValue::Int(2500000))];
        let encoded = encode_edge(99, &[4], &props, 0, 1, true);
        let decoded = decode_edge(&encoded);

        assert_eq!(decoded.node.user_id_str_id, 99);
        assert_eq!(decoded.node.label_str_ids, vec![4]);
        assert_eq!(decoded.node.props, vec![(7, PropValue::Int(2500000))]);
        assert_eq!(decoded.src_internal_id, 0);
        assert_eq!(decoded.tgt_internal_id, 1);
        assert!(decoded.directed);
    }

    #[test]
    fn test_undirected_edge() {
        let encoded = encode_edge(1, &[2], &[], 5, 6, false);
        let decoded = decode_edge(&encoded);
        assert!(!decoded.directed);
        assert_eq!(decoded.src_internal_id, 5);
        assert_eq!(decoded.tgt_internal_id, 6);
    }

    #[test]
    fn test_nested_null_roundtrip() {
        // Null inside a list and inside a record both round-trip via the
        // VALUE_TYPE_NULL tag; positional alignment is preserved.
        let inner_record = PropValue::Record(vec![
            (40, PropValue::Int(1)),
            (41, PropValue::Null),
            (42, PropValue::Int(3)),
        ]);
        let props = vec![(
            30,
            PropValue::List(vec![PropValue::Int(7), PropValue::Null, inner_record]),
        )];
        let encoded = encode_node(1, &[], &props);
        let (decoded, _) = decode_node(&encoded);
        assert_eq!(decoded.props, props);
    }
}
