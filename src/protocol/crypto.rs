//! AES-128-CBC session cipher and session-key derivation.

use aes::Aes128;
use cbc::cipher::{
    BlockModeDecrypt, BlockModeEncrypt, KeyIvInit,
    block_padding::{NoPadding, Pkcs7},
};
use md5::{Digest, Md5};
use zeroize::{Zeroize, ZeroizeOnDrop};

static KEY_TABLE: &[u8] = include_bytes!("keydata.bin");

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("key info reply too short ({0} bytes)")]
    ShortKeyInfo(usize),
    #[error("session key seed points outside the key table")]
    SeedOutOfRange,
}

pub fn md5(data: &[u8]) -> [u8; 16] {
    Md5::digest(data).into()
}

/// AES-128-CBC with PKCS#7 padding (the device's "type 7" encryption).
#[derive(Clone, ZeroizeOnDrop)]
pub struct SessionCipher {
    key: [u8; 16],
    iv: [u8; 16],
}

impl SessionCipher {
    pub fn new(key: [u8; 16], iv: [u8; 16]) -> Self {
        Self { key, iv }
    }

    /// Cipher used right after the ECDH exchange: key = shared[..16], iv = md5(shared).
    pub fn from_shared_secret(shared: &[u8; 20]) -> Self {
        let mut key = [0u8; 16];
        key.copy_from_slice(&shared[..16]);
        Self { key, iv: md5(shared) }
    }

    /// Same IV, new key (the session key replaces the ECDH-derived key).
    pub fn with_key(&self, key: [u8; 16]) -> Self {
        Self { key, iv: self.iv }
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; plaintext.len() + 16];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let len = cbc::Encryptor::<Aes128>::new(&self.key.into(), &self.iv.into())
            .encrypt_padded::<Pkcs7>(&mut buf, plaintext.len())
            .expect("buffer has room for one block of padding")
            .len();
        buf.truncate(len);
        buf
    }

    /// Decrypts whole blocks only (trailing partial bytes are dropped, as the device
    /// firmware does) and strips PKCS#7 padding when it is valid.
    pub fn decrypt(&self, ciphertext: &[u8]) -> Vec<u8> {
        let aligned = ciphertext.len() - ciphertext.len() % 16;
        if aligned == 0 {
            return ciphertext.to_vec();
        }
        let mut plain = ciphertext[..aligned].to_vec();
        cbc::Decryptor::<Aes128>::new(&self.key.into(), &self.iv.into())
            .decrypt_padded::<NoPadding>(&mut plain)
            .expect("aligned input cannot fail with NoPadding");
        if let Some(&n) = plain.last() {
            let n = n as usize;
            if (1..=16).contains(&n) && n <= plain.len() && plain[plain.len() - n..].iter().all(|&b| b as usize == n) {
                plain.truncate(plain.len() - n);
            }
        }
        plain
    }

    pub fn key_bytes(&self) -> [u8; 16] {
        self.key
    }
}

/// Derive the session key from the key-info reply (`srand` = 16 bytes, `seed` = 2 bytes).
pub fn derive_session_key(key_info: &[u8]) -> Result<[u8; 16], CryptoError> {
    if key_info.len() < 18 {
        return Err(CryptoError::ShortKeyInfo(key_info.len()));
    }
    let srand = &key_info[..16];
    let seed = &key_info[16..18];
    let pos = seed[0] as usize * 0x10 + (seed[1].wrapping_sub(1)) as usize * 0x100;
    let table = KEY_TABLE.get(pos..pos + 16).ok_or(CryptoError::SeedOutOfRange)?;
    let mut data = [0u8; 32];
    data[..16].copy_from_slice(table);
    data[16..].copy_from_slice(srand);
    let key = md5(&data);
    data.zeroize();
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_known_value() {
        assert_eq!(hex::encode(md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let c = SessionCipher::new([7; 16], [9; 16]);
        for len in [0usize, 1, 15, 16, 17, 100] {
            let data: Vec<u8> = (0..len as u8).collect();
            let ct = c.encrypt(&data);
            assert_eq!(ct.len() % 16, 0);
            assert_eq!(c.decrypt(&ct), data);
        }
    }

    #[test]
    fn decrypt_ignores_partial_trailing_block() {
        let c = SessionCipher::new([1; 16], [2; 16]);
        let mut ct = c.encrypt(b"hello world");
        ct.extend_from_slice(&[0xAA; 5]);
        assert_eq!(c.decrypt(&ct), b"hello world");
    }

    #[test]
    fn session_key_matches_reference_algorithm() {
        // Computed with the reference Python (keydata table + md5), see tests/fixtures/README.md.
        let mut info = vec![0u8; 18];
        for (i, b) in info.iter_mut().enumerate().take(16) {
            *b = i as u8;
        }
        info[16] = 0x03;
        info[17] = 0x05;
        let pos = 3 * 0x10 + 4 * 0x100;
        let mut buf = KEY_TABLE[pos..pos + 16].to_vec();
        buf.extend_from_slice(&info[..16]);
        assert_eq!(derive_session_key(&info).unwrap(), md5(&buf));
    }

    #[test]
    fn session_key_rejects_bad_input() {
        assert_eq!(derive_session_key(&[0; 5]), Err(CryptoError::ShortKeyInfo(5)));
        let mut info = vec![0u8; 18];
        info[16] = 0xFF;
        info[17] = 0xFF;
        assert_eq!(derive_session_key(&info), Err(CryptoError::SeedOutOfRange));
    }
}
