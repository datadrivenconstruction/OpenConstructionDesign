//! minisign-compatible signature check (Ed25519; "ED" = BLAKE2b-512 prehashed, "Ed" = legacy).
//! Public key text: base64("Ed" ‖ key_id[8] ‖ pk[32]). Signature file: untrusted comment line,
//! base64(alg[2] ‖ key_id[8] ‖ sig[64]), "trusted comment: …" line, base64(global_sig[64]).
//! The global signature covers sig ‖ trusted comment, so the comment cannot be swapped.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    pub key_id: [u8; 8],
    key: [u8; 32],
}

impl PublicKey {
    /// Accepts the bare base64 line or a whole `.pub` file (comment line + key line).
    pub fn parse(text: &str) -> Result<Self, String> {
        let line = text
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
            .ok_or("empty public key")?;
        let b = b64(line)?;
        if b.len() != 42 || &b[..2] != b"Ed" {
            return Err("not an Ed25519 public key".into());
        }
        Ok(Self {
            key_id: b[2..10].try_into().unwrap(),
            key: b[10..].try_into().unwrap(),
        })
    }

    pub fn from_raw(key_id: [u8; 8], key: [u8; 32]) -> Self {
        Self { key_id, key }
    }

    /// Ok(trusted comment) when `sig_text` is a valid signature of `data` by this key.
    pub fn verify(&self, data: &[u8], sig_text: &str) -> Result<String, String> {
        let lines: Vec<&str> = sig_text.lines().map(str::trim_end).collect();
        if lines.len() < 4 || !lines[0].starts_with("untrusted comment:") {
            return Err("signature file layout".into());
        }
        let s = b64(lines[1])?;
        if s.len() != 74 {
            return Err("signature length".into());
        }
        if s[2..10] != self.key_id {
            return Err("signed by a different key".into());
        }
        let vk = VerifyingKey::from_bytes(&self.key).map_err(|_| "bad public key")?;
        let sig = Signature::from_bytes(s[10..74].try_into().unwrap());
        let ok = match &s[..2] {
            b"ED" => vk.verify(&blake2b512(data), &sig),
            b"Ed" => vk.verify(data, &sig),
            _ => return Err("unknown signature algorithm".into()),
        };
        ok.map_err(|_| "signature does not match file")?;
        let trusted = lines[2]
            .strip_prefix("trusted comment: ")
            .ok_or("trusted comment line")?;
        let g = b64(lines[3])?;
        let g: [u8; 64] = g.try_into().map_err(|_| "global signature length")?;
        let mut msg = s[10..74].to_vec();
        msg.extend_from_slice(trusted.as_bytes());
        vk.verify(&msg, &Signature::from_bytes(&g))
            .map_err(|_| "trusted comment signature does not match")?;
        Ok(trusted.to_string())
    }
}

pub fn b64(s: &str) -> Result<Vec<u8>, String> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    // strict: padded to a multiple of 4, '=' only at the end, unused bits zero
    let s = s.trim().as_bytes();
    if s.is_empty() || !s.len().is_multiple_of(4) {
        return Err("invalid base64 length".into());
    }
    let pad = s.iter().rev().take_while(|&&c| c == b'=').count();
    if pad > 2 {
        return Err("invalid base64 padding".into());
    }
    let body = &s[..s.len() - pad];
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in body {
        acc = (acc << 6) | val(c).ok_or("invalid base64")?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if acc & ((1 << bits) - 1) != 0 {
        return Err("invalid base64 trailing bits".into());
    }
    Ok(out)
}

pub fn b64_encode(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for ch in b.chunks(3) {
        let n = (ch[0] as u32) << 16
            | (*ch.get(1).unwrap_or(&0) as u32) << 8
            | *ch.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= ch.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

// ---------- BLAKE2b-512 (RFC 7693) ----------

const IV: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];
const SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

fn compress(h: &mut [u64; 8], block: &[u8; 128], t: u128, last: bool) {
    let m: [u64; 16] =
        std::array::from_fn(|i| u64::from_le_bytes(block[i * 8..i * 8 + 8].try_into().unwrap()));
    let mut v = [0u64; 16];
    v[..8].copy_from_slice(h);
    v[8..].copy_from_slice(&IV);
    v[12] ^= t as u64;
    v[13] ^= (t >> 64) as u64;
    if last {
        v[14] = !v[14];
    }
    fn g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
        v[d] = (v[d] ^ v[a]).rotate_right(32);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(24);
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
        v[d] = (v[d] ^ v[a]).rotate_right(16);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(63);
    }
    for r in 0..12 {
        let s = &SIGMA[r % 10];
        g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }
    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }
}

pub fn blake2b512(data: &[u8]) -> [u8; 64] {
    let mut h = IV;
    h[0] ^= 0x0101_0000 ^ 64;
    let mut t: u128 = 0;
    let mut chunks = data.chunks(128).peekable();
    if data.is_empty() {
        compress(&mut h, &[0; 128], 0, true);
    }
    while let Some(c) = chunks.next() {
        let mut block = [0u8; 128];
        block[..c.len()].copy_from_slice(c);
        t += c.len() as u128;
        compress(&mut h, &block, t, chunks.peek().is_none());
    }
    let mut out = [0u8; 64];
    for (i, w) in h.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn blake2b_rfc7693_vectors() {
        assert_eq!(
            hex(&blake2b512(b"abc")),
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1\
             7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
        );
        assert_eq!(
            hex(&blake2b512(b"")),
            "786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419\
             d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce"
        );
        // block boundary: 128 and 129 bytes hash differently and do not panic
        assert_ne!(blake2b512(&[7; 128]), blake2b512(&[7; 129]));
    }

    /// Test signer producing the same text layout as the reference minisign tool.
    pub(crate) fn keypair(seed: u8) -> (SigningKey, PublicKey, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let id = [seed, 1, 2, 3, 4, 5, 6, 7];
        let pk = PublicKey::from_raw(id, sk.verifying_key().to_bytes());
        let mut raw = b"Ed".to_vec();
        raw.extend_from_slice(&id);
        raw.extend_from_slice(&sk.verifying_key().to_bytes());
        let text = format!("untrusted comment: test key\n{}\n", b64_encode(&raw));
        (sk, pk, text)
    }

    pub(crate) fn sign(sk: &SigningKey, id: [u8; 8], data: &[u8], trusted: &str) -> String {
        let sig = sk.sign(&blake2b512(data)).to_bytes();
        let mut s = b"ED".to_vec();
        s.extend_from_slice(&id);
        s.extend_from_slice(&sig);
        let mut g = sig.to_vec();
        g.extend_from_slice(trusted.as_bytes());
        let gs = sk.sign(&g).to_bytes();
        format!(
            "untrusted comment: signature\n{}\ntrusted comment: {trusted}\n{}\n",
            b64_encode(&s),
            b64_encode(&gs)
        )
    }

    #[test]
    fn verify_accepts_valid_and_rejects_tampering() {
        let (sk, pk, text) = keypair(9);
        assert_eq!(PublicKey::parse(&text).unwrap(), pk);
        let data = b"bridge executable bytes";
        let sig = sign(&sk, pk.key_id, data, "file:bridge.exe");
        assert_eq!(pk.verify(data, &sig).unwrap(), "file:bridge.exe");
        assert!(pk.verify(b"bridge executable bytez", &sig).is_err());
        let swapped = sig.replace("file:bridge.exe", "file:other.exe");
        assert!(pk.verify(data, &swapped).is_err());
        let (_, other, _) = keypair(10);
        assert!(other.verify(data, &sig).is_err());
        assert!(pk.verify(data, "garbage").is_err());
        assert_eq!(b64(&b64_encode(b"ab")).unwrap(), b"ab");
        assert!(b64("YWI").is_err(), "unpadded");
        assert!(b64("YWJ=").is_err(), "non-zero trailing bits");
        assert!(b64("Y=Ji").is_err() && b64("YQ===").is_err() && b64("YW I=").is_err());
        assert_eq!(b64("YWI=").unwrap(), b"ab");
    }
}
