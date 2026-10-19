//! New wallets and recovery phrases, for the first-start wizard.
//!
//! - `generate`: 32 random bytes from the OS (getrandom), a valid secp256k1 key.
//! - `from_phrase`: a BIP-39 recovery phrase (12 to 24 words, optional passphrase) to the key at
//!   the standard Ethereum path m/44'/60'/0'/0/<index> (BIP-32), the one MetaMask, Rabby and
//!   hardware wallets use for their first accounts.
//!
//! The words are not checked against the BIP-39 word list (the app does not ship it): a mistyped
//! phrase gives a different, valid wallet. The wizard therefore shows the addresses the phrase
//! gives and the user picks theirs. Nothing here stores or logs a key.

use hmac::{Hmac, Mac};
use k256::elliptic_curve::ff::PrimeField;
use k256::elliptic_curve::sec1::ToEncodedPoint;
use k256::{Scalar, SecretKey};
use sha2::Sha512;

type HmacSha512 = Hmac<Sha512>;

fn hmac512(key: &[u8], data: &[u8]) -> [u8; 64] {
    let mut m = HmacSha512::new_from_slice(key).expect("any key length");
    m.update(data);
    m.finalize().into_bytes().into()
}

/// PBKDF2-HMAC-SHA512, one 64-byte block (all BIP-39 needs).
fn pbkdf2_512(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 64] {
    let mut s = salt.to_vec();
    s.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac512(password, &s);
    let mut out = u;
    for _ in 1..rounds {
        u = hmac512(password, &u);
        for (o, x) in out.iter_mut().zip(u.iter()) {
            *o ^= x;
        }
    }
    out
}

/// The phrase as BIP-39 hashes it: lowercase words separated by single spaces.
pub fn normalise_phrase(p: &str) -> Result<String, String> {
    let words: Vec<String> = p.split_whitespace().map(|w| w.to_lowercase()).collect();
    if ![12, 15, 18, 21, 24].contains(&words.len()) {
        return Err(format!("a recovery phrase has 12, 15, 18, 21 or 24 words (this one has {})", words.len()));
    }
    if let Some(w) = words.iter().find(|w| !w.chars().all(|c| c.is_ascii_lowercase())) {
        return Err(format!("\"{w}\" is not a recovery phrase word (English words, letters only)"));
    }
    Ok(words.join(" "))
}

fn child(k: &Scalar, chain: &[u8], index: u32) -> Result<(Scalar, [u8; 32]), String> {
    let mut data = Vec::with_capacity(37);
    if index >= 0x8000_0000 {
        data.push(0);
        data.extend_from_slice(&k.to_repr());
    } else {
        let sk = SecretKey::from_bytes(&k.to_repr()).map_err(|_| "invalid key")?;
        data.extend_from_slice(sk.public_key().to_encoded_point(true).as_bytes());
    }
    data.extend_from_slice(&index.to_be_bytes());
    let i = hmac512(chain, &data);
    let il: Option<Scalar> = Scalar::from_repr(*k256::FieldBytes::from_slice(&i[..32])).into();
    let il = il.ok_or("derivation hit an invalid key (try the next account)")?;
    let c = il + k;
    if bool::from(c.is_zero()) {
        return Err("derivation hit an invalid key (try the next account)".into());
    }
    let mut cc = [0u8; 32];
    cc.copy_from_slice(&i[32..]);
    Ok((c, cc))
}

/// The private key (0x hex) of account `index` of a recovery phrase.
pub fn from_phrase(phrase: &str, passphrase: &str, index: u32) -> Result<String, String> {
    let p = normalise_phrase(phrase)?;
    let seed = pbkdf2_512(p.as_bytes(), format!("mnemonic{passphrase}").as_bytes(), 2048);
    let m = hmac512(b"Bitcoin seed", &seed);
    let k: Option<Scalar> = Scalar::from_repr(*k256::FieldBytes::from_slice(&m[..32])).into();
    let mut k = k.ok_or("this phrase gives an invalid master key")?;
    let mut chain = [0u8; 32];
    chain.copy_from_slice(&m[32..]);
    for i in [44 | 0x8000_0000, 60 | 0x8000_0000, 0x8000_0000, 0, index] {
        let (nk, nc) = child(&k, &chain, i)?;
        k = nk;
        chain = nc;
    }
    Ok(format!("0x{}", crate::eth::hex(&k.to_repr())))
}

/// A new random private key (0x hex) and its address.
pub fn generate() -> Result<(String, String), String> {
    for _ in 0..8 {
        let mut b = [0u8; 32];
        getrandom::getrandom(&mut b).map_err(|e| format!("no randomness from the OS: {e}"))?;
        let k = format!("0x{}", crate::eth::hex(&b));
        if let Ok(a) = crate::eth::Account::from_key(&k) {
            return Ok((k, a.address));
        }
    }
    Err("could not make a key".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_test_phrase() {
        // the well-known BIP-39 test phrase and its first Ethereum account
        let p = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let k = from_phrase(p, "", 0).unwrap();
        assert_eq!(crate::eth::Account::from_key(&k).unwrap().address, "0x9858EfFD232B4033E47d90003D41EC34EcaEda94");
        // spacing and case do not matter
        let k2 = from_phrase(&p.to_uppercase().replace(' ', "  "), "", 0).unwrap();
        assert_eq!(k, k2);
        assert_ne!(from_phrase(p, "", 1).unwrap(), k);
        assert!(normalise_phrase("one two three").is_err());
        assert!(normalise_phrase(&"word1 ".repeat(12)).is_err());
    }

    #[test]
    fn new_keys_differ_and_work() {
        let (a, aa) = generate().unwrap();
        let (b, _) = generate().unwrap();
        assert_ne!(a, b);
        assert_eq!(crate::eth::Account::from_key(&a).unwrap().address, aa);
    }
}
