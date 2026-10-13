// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from nhimc0n/ptouch-yuri, commit 10e14876 (see NOTICE).
use crate::{PtouchError, Result};
/// A decoded SNMP value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SnmpValue {
    Int(i64),
    Bytes(Vec<u8>),
    Null,
    NoSuch,
}

fn ber_tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match body.len() {
        n if n < 0x80 => out.push(n as u8),
        n if n <= 0xFF => out.extend([0x81, n as u8]),
        n => out.extend([0x82, (n >> 8) as u8, n as u8]),
    }
    out.extend_from_slice(body);
    out
}

fn ber_int(value: i64) -> Vec<u8> {
    let mut bytes = value.to_be_bytes().to_vec();
    while bytes.len() > 1
        && ((bytes[0] == 0x00 && bytes[1] & 0x80 == 0)
            || (bytes[0] == 0xFF && bytes[1] & 0x80 != 0))
    {
        bytes.remove(0);
    }
    ber_tlv(0x02, &bytes)
}

fn encode_oid(oid: &str) -> Result<Vec<u8>> {
    let arcs: Vec<u32> = oid
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| PtouchError::StatusError(format!("Invalid OID {oid}")))?;
    if arcs.len() < 2 || arcs[0] > 2 || arcs[1] >= 40 {
        return Err(PtouchError::StatusError(format!("Invalid OID {oid}")));
    }
    let mut body = vec![(arcs[0] * 40 + arcs[1]) as u8];
    for &arc in &arcs[2..] {
        let mut groups = vec![(arc & 0x7F) as u8];
        let mut rest = arc >> 7;
        while rest > 0 {
            groups.push((rest & 0x7F) as u8 | 0x80);
            rest >>= 7;
        }
        groups.reverse();
        body.extend(groups);
    }
    Ok(ber_tlv(0x06, &body))
}

/// Build an SNMPv1 GetRequest.
pub(crate) fn snmp_get_request(community: &str, request_id: i32, oids: &[&str]) -> Result<Vec<u8>> {
    let mut varbinds = Vec::new();
    for oid in oids {
        let mut pair = encode_oid(oid)?;
        pair.extend([0x05, 0x00]); // NULL
        varbinds.extend(ber_tlv(0x30, &pair));
    }
    let mut pdu = ber_int(i64::from(request_id));
    pdu.extend(ber_int(0)); // error status
    pdu.extend(ber_int(0)); // error index
    pdu.extend(ber_tlv(0x30, &varbinds));
    let mut message = ber_int(0); // SNMP v1
    message.extend(ber_tlv(0x04, community.as_bytes()));
    message.extend(ber_tlv(0xA0, &pdu));
    Ok(ber_tlv(0x30, &message))
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn expected(&mut self, tag: u8) -> Result<&'a [u8]> {
        let (found, body) = self.tlv()?;
        if found != tag {
            return Err(PtouchError::StatusError(
                "Unexpected SNMP field type".into(),
            ));
        }
        Ok(body)
    }

    fn tlv(&mut self) -> Result<(u8, &'a [u8])> {
        let bad = || PtouchError::StatusError("Malformed SNMP reply".into());
        let tag = *self.data.get(self.pos).ok_or_else(bad)?;
        let first = *self.data.get(self.pos + 1).ok_or_else(bad)?;
        let (len, header) = if first & 0x80 == 0 {
            (usize::from(first), 2)
        } else {
            let count = usize::from(first & 0x7F);
            if count == 0 || count > 2 {
                return Err(bad());
            }
            let bytes = self
                .data
                .get(self.pos + 2..self.pos + 2 + count)
                .ok_or_else(bad)?;
            (
                bytes
                    .iter()
                    .fold(0usize, |acc, b| (acc << 8) | usize::from(*b)),
                2 + count,
            )
        };
        let start = self.pos + header;
        let body = self.data.get(start..start + len).ok_or_else(bad)?;
        self.pos = start + len;
        Ok((tag, body))
    }
}

fn decode_int(body: &[u8]) -> i64 {
    let mut value = if body.first().is_some_and(|b| b & 0x80 != 0) {
        -1i64
    } else {
        0
    };
    for b in body {
        value = (value << 8) | i64::from(*b);
    }
    value
}

fn decode_oid(body: &[u8]) -> String {
    let Some((&first, rest)) = body.split_first() else {
        return String::new();
    };
    let mut arcs = vec![u64::from(first / 40), u64::from(first % 40)];
    let mut acc = 0u64;
    for b in rest {
        acc = (acc << 7) | u64::from(b & 0x7F);
        if b & 0x80 == 0 {
            arcs.push(acc);
            acc = 0;
        }
    }
    arcs.iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// Parse a complete SNMPv1 public GetResponse into its request id and values.
pub(crate) fn snmp_parse(packet: &[u8]) -> Result<(i32, Vec<(String, SnmpValue)>)> {
    let bad = || PtouchError::StatusError("Malformed SNMP reply".into());
    let mut envelope = Reader::new(packet);
    let message = envelope.expected(0x30)?;
    if !envelope.done() {
        return Err(bad());
    }
    let mut msg = Reader::new(message);
    let version = msg.expected(0x02)?;
    if version != [0] || msg.expected(0x04)? != b"public" {
        return Err(bad());
    }
    let pdu = msg.expected(0xa2)?;
    if !msg.done() {
        return Err(bad());
    }
    let mut pdu = Reader::new(pdu);
    let id = pdu.expected(0x02)?;
    let error_status = pdu.expected(0x02)?;
    let error_index = pdu.expected(0x02)?;
    if id.is_empty()
        || id.len() > 4
        || error_status.is_empty()
        || error_status.len() > 4
        || error_index.is_empty()
        || error_index.len() > 4
    {
        return Err(bad());
    }
    if decode_int(error_status) != 0 {
        return Err(PtouchError::StatusError(format!(
            "SNMP error status {}",
            decode_int(error_status)
        )));
    }
    let varbinds = pdu.expected(0x30)?;
    if !pdu.done() {
        return Err(bad());
    }
    let mut vbs = Reader::new(varbinds);
    let mut values = Vec::new();
    while !vbs.done() {
        let pair = vbs.expected(0x30)?;
        let mut pair = Reader::new(pair);
        let (oid_tag, oid) = pair.tlv()?;
        if oid_tag != 0x06 {
            return Err(bad());
        }
        let (tag, body) = pair.tlv()?;
        if !pair.done()
            || oid.is_empty()
            || oid.last().is_some_and(|b| b & 0x80 != 0)
            || (matches!(tag, 0x02 | 0x41..=0x43) && (body.is_empty() || body.len() > 8))
        {
            return Err(bad());
        }
        let value = match tag {
            0x02 => SnmpValue::Int(decode_int(body)),
            0x04 => SnmpValue::Bytes(body.to_vec()),
            0x41..=0x43 => SnmpValue::Int(body.iter().fold(0i64, |a, b| (a << 8) | i64::from(*b))),
            0x80..=0x82 => SnmpValue::NoSuch,
            _ => SnmpValue::Null,
        };
        values.push((decode_oid(oid), value));
    }
    Ok((decode_int(id) as i32, values))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_response_and_rejects_every_truncated_prefix() {
        let mut vb = encode_oid("1.3.6.1.2.1.25.3.5.1.1.1").unwrap();
        vb.extend(ber_int(4));
        let mut pdu = ber_int(1234);
        pdu.extend(ber_int(0));
        pdu.extend(ber_int(0));
        pdu.extend(ber_tlv(0x30, &ber_tlv(0x30, &vb)));
        let mut body = ber_int(0);
        body.extend(ber_tlv(4, b"public"));
        body.extend(ber_tlv(0xa2, &pdu));
        let response = ber_tlv(0x30, &body);
        assert_eq!(
            snmp_parse(&response).unwrap(),
            (
                1234,
                vec![("1.3.6.1.2.1.25.3.5.1.1.1".into(), SnmpValue::Int(4))]
            )
        );
        for end in 0..response.len() {
            assert!(snmp_parse(&response[..end]).is_err());
        }
        assert!(
            snmp_parse(&snmp_get_request("public", 1234, &["1.3.6.1.2.1.25.3.5.1.1.1"]).unwrap())
                .is_err()
        );
    }
}
