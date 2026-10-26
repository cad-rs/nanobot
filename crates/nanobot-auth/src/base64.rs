//! Minimal base64url (RFC 4648 §5, unpadded) — one copy for the crate:
//! the license token codec and the JWT-claim decoder share it. Hand-rolled
//! to keep the dependency face at `ed25519-dalek` + `serde` alone.

const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub(crate) fn encode_urlsafe(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let group = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let quad = [
            (group >> 18) as usize & 0x3f,
            (group >> 12) as usize & 0x3f,
            (group >> 6) as usize & 0x3f,
            group as usize & 0x3f,
        ];
        let significant = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for (i, index) in quad.iter().enumerate() {
            if i < significant {
                out.push(TABLE[*index] as char);
            }
        }
    }
    out
}

pub(crate) fn decode_urlsafe(encoded: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(encoded.len() * 3 / 4);
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    for ch in encoded.trim_end_matches('=').bytes() {
        let value = TABLE.iter().position(|c| *c == ch)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_on_split_boundaries() {
        for len in 0..40 {
            let data: Vec<u8> = (0..len as u8).collect();
            assert_eq!(
                decode_urlsafe(&encode_urlsafe(&data)),
                Some(data.clone()),
                "len {len}"
            );
        }
    }

    #[test]
    fn decodes_a_known_vector() {
        // "Hello" in base64url, unpadded.
        assert_eq!(
            decode_urlsafe("SGVsbG8"),
            Some(b"Hello".to_vec())
        );
    }
}
