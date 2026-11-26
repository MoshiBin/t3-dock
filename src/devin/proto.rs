//! Just enough protobuf for one request and the few fields read from its response.

pub fn varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let low = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

/// A length-delimited field: a string or an embedded message.
pub fn bytes_field(number: u32, value: &[u8], out: &mut Vec<u8>) {
    varint(u64::from(number) << 3 | 2, out);
    varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64([u8; 8]),
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
}

fn read_varint(data: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *data.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// The fields of one message, in wire order. `None` if the message is malformed.
pub fn fields(data: &[u8]) -> Option<Vec<(u32, Value<'_>)>> {
    let mut at = 0;
    let mut fields = Vec::new();
    while at < data.len() {
        let key = read_varint(data, &mut at)?;
        let number = u32::try_from(key >> 3).ok()?;
        let value = match key & 7 {
            0 => Value::Varint(read_varint(data, &mut at)?),
            1 => {
                let bytes = data.get(at..at + 8)?;
                at += 8;
                Value::Fixed64(bytes.try_into().ok()?)
            }
            2 => {
                let length = usize::try_from(read_varint(data, &mut at)?).ok()?;
                let bytes = data.get(at..at.checked_add(length)?)?;
                at += length;
                Value::Bytes(bytes)
            }
            5 => {
                let bytes = data.get(at..at + 4)?;
                at += 4;
                Value::Fixed32(bytes.try_into().ok()?)
            }
            _ => return None,
        };
        fields.push((number, value));
    }
    Some(fields)
}

/// The last occurrence of a field wins, as in protobuf.
pub fn find<'a>(fields: &[(u32, Value<'a>)], number: u32) -> Option<Value<'a>> {
    fields
        .iter()
        .rev()
        .find(|(n, _)| *n == number)
        .map(|(_, value)| *value)
}

pub fn message<'a>(fields: &[(u32, Value<'a>)], number: u32) -> Option<Vec<(u32, Value<'a>)>> {
    match find(fields, number)? {
        Value::Bytes(bytes) => self::fields(bytes),
        _ => None,
    }
}

/// A `double`, which is zero when absent.
pub fn double(fields: &[(u32, Value<'_>)], number: u32) -> Option<f64> {
    match find(fields, number) {
        Some(Value::Fixed64(bytes)) => Some(f64::from_le_bytes(bytes)),
        None => Some(0.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_nested_fields() {
        let mut inner = Vec::new();
        varint(19 << 3 | 1, &mut inner);
        inner.extend_from_slice(&1270.5f64.to_le_bytes());
        let mut outer = Vec::new();
        bytes_field(13, &inner, &mut outer);
        varint(1 << 3, &mut outer);
        varint(300, &mut outer);

        let top = fields(&outer).unwrap();
        assert_eq!(find(&top, 1), Some(Value::Varint(300)));
        let plan = message(&top, 13).unwrap();
        assert_eq!(double(&plan, 19), Some(1270.5));
        assert_eq!(double(&plan, 20), Some(0.0));
    }

    #[test]
    fn malformed_messages_are_refused() {
        assert_eq!(fields(&[0x0a, 5, 1]), None);
        assert_eq!(fields(&[0xff]), None);
        assert_eq!(fields(&[0x0b]), None);
    }
}
