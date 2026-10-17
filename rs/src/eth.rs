//! Ethereum primitives: keccak, keys and addresses, ABI encoding, RLP and EIP-1559 signing.

use k256::ecdsa::SigningKey;
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{ToPrimitive, Zero};
use tiny_keccak::{Hasher, Keccak};

pub fn keccak(data: &[u8]) -> [u8; 32] {
    let mut k = Keccak::v256();
    let mut out = [0u8; 32];
    k.update(data);
    k.finalize(&mut out);
    out
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

pub fn unhex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string())).collect()
}

pub fn selector(sig: &str) -> [u8; 4] {
    let h = keccak(sig.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

pub fn to_checksum(addr: &str) -> String {
    let a = addr.trim_start_matches("0x").to_lowercase();
    let h = hex(&keccak(a.as_bytes()));
    let mut out = String::from("0x");
    for (c, hc) in a.chars().zip(h.chars()) {
        if c.is_ascii_alphabetic() && hc.to_digit(16).unwrap() >= 8 {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

// -- keys --------------------------------------------------------------------------------------------
pub struct Account {
    key: SigningKey,
    pub address: String, // checksummed
}

impl Account {
    pub fn from_key(key: &str) -> Result<Account, String> {
        let raw = unhex(key.trim())?;
        if raw.len() != 32 {
            return Err("private key must be 32 bytes".into());
        }
        let key = SigningKey::from_slice(&raw).map_err(|_| "invalid private key".to_string())?;
        let pubkey = key.verifying_key().to_encoded_point(false);
        let h = keccak(&pubkey.as_bytes()[1..]);
        Ok(Account { address: to_checksum(&hex(&h[12..])), key })
    }

    /// (y_parity, r, s) of a recoverable signature over a 32-byte hash (low-s normalised).
    pub fn sign_hash(&self, hash: &[u8; 32]) -> (u8, [u8; 32], [u8; 32]) {
        let (sig, rec) = self.key.sign_prehash_recoverable(hash).expect("signing");
        let (sig, rec) = match sig.normalize_s() {
            Some(n) => (n, k256::ecdsa::RecoveryId::new(!rec.is_y_odd(), rec.is_x_reduced())),
            None => (sig, rec),
        };
        let b = sig.to_bytes();
        let mut r = [0u8; 32];
        let mut s = [0u8; 32];
        r.copy_from_slice(&b[..32]);
        s.copy_from_slice(&b[32..]);
        (rec.is_y_odd() as u8, r, s)
    }
}

// -- ABI ---------------------------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub enum Tok {
    Addr(String),
    Uint(BigUint),
    Int(BigInt),
    B32([u8; 32]),
    Bool(bool),
    Arr(Vec<Tok>),
    Tup(Vec<Tok>),
}

pub fn u(v: impl Into<BigUint>) -> Tok {
    Tok::Uint(v.into())
}
pub fn i(v: i64) -> Tok {
    Tok::Int(BigInt::from(v))
}
pub fn addr(a: &str) -> Tok {
    Tok::Addr(a.to_string())
}

fn dynamic(t: &Tok) -> bool {
    match t {
        Tok::Arr(_) => true,
        Tok::Tup(x) => x.iter().any(dynamic),
        _ => false,
    }
}

fn word_uint(v: &BigUint) -> [u8; 32] {
    let b = v.to_bytes_be();
    assert!(b.len() <= 32, "uint overflows 256 bits");
    let mut w = [0u8; 32];
    w[32 - b.len()..].copy_from_slice(&b);
    w
}

fn word_int(v: &BigInt) -> [u8; 32] {
    if v.sign() != Sign::Minus {
        return word_uint(&v.to_biguint().unwrap());
    }
    let m: BigInt = (BigInt::from(1) << 256usize) + v;
    word_uint(&m.to_biguint().unwrap())
}

fn enc_one(t: &Tok) -> Vec<u8> {
    match t {
        Tok::Addr(a) => {
            let b = unhex(a).expect("address hex");
            let mut w = [0u8; 32];
            w[12..].copy_from_slice(&b);
            w.to_vec()
        }
        Tok::Uint(v) => word_uint(v).to_vec(),
        Tok::Int(v) => word_int(v).to_vec(),
        Tok::B32(b) => b.to_vec(),
        Tok::Bool(b) => word_uint(&BigUint::from(*b as u8)).to_vec(),
        Tok::Arr(items) => {
            let mut out = word_uint(&BigUint::from(items.len())).to_vec();
            out.extend(encode(items));
            out
        }
        Tok::Tup(items) => encode(items),
    }
}

/// Standard ABI encoding of a parameter list (head/tail).
pub fn encode(toks: &[Tok]) -> Vec<u8> {
    let heads: usize = toks.iter().map(|t| if dynamic(t) { 32 } else { enc_one(t).len() }).sum();
    let mut head = Vec::new();
    let mut tail = Vec::new();
    for t in toks {
        if dynamic(t) {
            head.extend(word_uint(&BigUint::from(heads + tail.len())));
            tail.extend(enc_one(t));
        } else {
            head.extend(enc_one(t));
        }
    }
    head.extend(tail);
    head
}

pub fn calldata(sig: &str, toks: &[Tok]) -> String {
    let mut b = selector(sig).to_vec();
    b.extend(encode(toks));
    format!("0x{}", hex(&b))
}

/// Word i of ABI-encoded return data.
pub fn word(data: &[u8], i: usize) -> &[u8] {
    &data[i * 32..i * 32 + 32]
}
pub fn word_u(data: &[u8], i: usize) -> BigUint {
    BigUint::from_bytes_be(word(data, i))
}
pub fn word_i(data: &[u8], i: usize) -> BigInt {
    let v = BigInt::from_bytes_be(Sign::Plus, word(data, i));
    if data[i * 32] & 0x80 != 0 {
        v - (BigInt::from(1) << 256)
    } else {
        v
    }
}
pub fn decode_string(data: &[u8]) -> Option<String> {
    let off = word_u(data, 0).to_usize()?;
    let len = BigUint::from_bytes_be(data.get(off..off + 32)?).to_usize()?;
    String::from_utf8(data.get(off + 32..off + 32 + len)?.to_vec()).ok()
}

pub fn parse_uint(s: &str) -> BigUint {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") {
        BigUint::parse_bytes(if h.is_empty() { b"0" } else { h.as_bytes() }, 16).unwrap_or_default()
    } else {
        BigUint::parse_bytes(s.as_bytes(), 10).unwrap_or_default()
    }
}

// -- RLP and EIP-1559 ----------------------------------------------------------------------------------
pub enum Rlp {
    B(Vec<u8>),
    L(Vec<Rlp>),
}

fn rlp_len(prefix: u8, n: usize) -> Vec<u8> {
    if n < 56 {
        vec![prefix + n as u8]
    } else {
        let b = BigUint::from(n).to_bytes_be();
        let mut v = vec![prefix + 55 + b.len() as u8];
        v.extend(b);
        v
    }
}

pub fn rlp(x: &Rlp) -> Vec<u8> {
    match x {
        Rlp::B(b) if b.len() == 1 && b[0] < 0x80 => b.clone(),
        Rlp::B(b) => {
            let mut v = rlp_len(0x80, b.len());
            v.extend(b);
            v
        }
        Rlp::L(items) => {
            let body: Vec<u8> = items.iter().flat_map(rlp).collect();
            let mut v = rlp_len(0xc0, body.len());
            v.extend(body);
            v
        }
    }
}

pub fn rlp_uint(v: &BigUint) -> Rlp {
    if v.is_zero() {
        Rlp::B(vec![])
    } else {
        Rlp::B(v.to_bytes_be())
    }
}

pub struct Tx1559 {
    pub chain_id: u64,
    pub nonce: u64,
    pub max_priority_fee: BigUint,
    pub max_fee: BigUint,
    pub gas: u64,
    pub to: String,
    pub data: Vec<u8>,
}

impl Tx1559 {
    fn fields(&self) -> Vec<Rlp> {
        vec![
            rlp_uint(&BigUint::from(self.chain_id)),
            rlp_uint(&BigUint::from(self.nonce)),
            rlp_uint(&self.max_priority_fee),
            rlp_uint(&self.max_fee),
            rlp_uint(&BigUint::from(self.gas)),
            Rlp::B(unhex(&self.to).expect("to")),
            Rlp::B(vec![]),
            Rlp::B(self.data.clone()),
            Rlp::L(vec![]),
        ]
    }

    /// Signed raw transaction (0x02 || rlp([...fields, yParity, r, s])).
    pub fn sign(&self, acct: &Account) -> Vec<u8> {
        let mut pre = vec![0x02u8];
        pre.extend(rlp(&Rlp::L(self.fields())));
        let (y, r, s) = acct.sign_hash(&keccak(&pre));
        let mut f = self.fields();
        f.push(rlp_uint(&BigUint::from(y)));
        f.push(rlp_uint(&BigUint::from_bytes_be(&r)));
        f.push(rlp_uint(&BigUint::from_bytes_be(&s)));
        let mut out = vec![0x02u8];
        out.extend(rlp(&Rlp::L(f)));
        out
    }
}
