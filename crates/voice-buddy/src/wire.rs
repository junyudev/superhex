//! Deliberately presence-preserving protobuf: zero scalars in the DH and
//! outer envelopes are required even though proto3 serializers omit them.
use crate::Failure;

pub(crate) const MAX_MESSAGE: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy)]
pub(crate) enum Value<'a> {
    Int(u64),
    Bytes(&'a [u8]),
}
use Value::*;
pub(crate) type Fields<'a> = Vec<(u32, Value<'a>)>;

pub(crate) fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value > 127 {
        out.push(value as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
pub(crate) fn message(fields: &[(u32, Value<'_>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(number, value) in fields {
        varint(
            u64::from(number) << 3 | if matches!(value, Bytes(_)) { 2 } else { 0 },
            &mut out,
        );
        match value {
            Int(n) => varint(n, &mut out),
            Bytes(b) => {
                varint(b.len() as u64, &mut out);
                out.extend_from_slice(b);
            }
        }
    }
    out
}
fn read_varint(data: &[u8], pos: &mut usize) -> Result<u64, Failure> {
    let mut n = 0;
    for i in 0..10 {
        let b = *data.get(*pos).ok_or_else(malformed)?;
        *pos += 1;
        if i == 9 && b > 1 {
            return Err(malformed());
        }
        n |= u64::from(b & 127) << (7 * i);
        if b & 128 == 0 {
            return Ok(n);
        }
    }
    Err(malformed())
}
pub(crate) fn decode(data: &[u8]) -> Result<Fields<'_>, Failure> {
    if data.len() > MAX_MESSAGE {
        return Err(malformed());
    }
    let mut fields = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        if fields.len() >= 65_536 {
            return Err(malformed());
        }
        let tag = read_varint(data, &mut pos)?;
        let number = u32::try_from(tag >> 3).map_err(|_| malformed())?;
        if number == 0 || number > 0x1fff_ffff {
            return Err(malformed());
        }
        if tag & 7 == 0 {
            fields.push((number, Int(read_varint(data, &mut pos)?)));
            continue;
        }
        let length = match tag & 7 {
            1 => 8,
            5 => 4,
            2 => usize::try_from(read_varint(data, &mut pos)?).map_err(|_| malformed())?,
            _ => return Err(malformed()),
        };
        let end = pos
            .checked_add(length)
            .filter(|e| *e <= data.len())
            .ok_or_else(malformed)?;
        fields.push((number, Bytes(&data[pos..end])));
        pos = end;
    }
    Ok(fields)
}
pub(crate) fn bytes<'a>(fields: &Fields<'a>, number: u32) -> Result<&'a [u8], Failure> {
    match fields
        .iter()
        .rev()
        .find(|(n, _)| *n == number)
        .map(|(_, v)| v)
    {
        Some(Bytes(b)) => Ok(b),
        None => Ok(&[]),
        _ => Err(malformed()),
    }
}
pub(crate) fn int(fields: &Fields<'_>, number: u32) -> Result<u64, Failure> {
    match fields
        .iter()
        .rev()
        .find(|(n, _)| *n == number)
        .map(|(_, v)| v)
    {
        Some(Int(n)) => Ok(*n),
        None => Ok(0),
        _ => Err(malformed()),
    }
}
pub(crate) fn text<'a>(fields: &Fields<'a>, number: u32) -> Result<&'a str, Failure> {
    std::str::from_utf8(bytes(fields, number)?).map_err(|_| malformed())
}
pub(crate) fn malformed() -> Failure {
    Failure::Internal("malformed service response".into())
}
