use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use domain::{Error, Result};
use rand::RngCore;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"LDSREC01";
const CHUNK: usize = 1024 * 1024;
const MAX_BYTES: usize = 512 * 1024 * 1024;

pub fn seal(key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0; 24];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let encrypted = XChaCha20Poly1305::new(key.into())
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
        .map_err(|_| Error::Authentication)?;
    let mut output = nonce.to_vec();
    output.extend(encrypted);
    Ok(output)
}
pub fn open(key: &[u8; 32], data: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 40 {
        return Err(Error::Authentication);
    }
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(&data[..24]),
            Payload {
                msg: &data[24..],
                aad,
            },
        )
        .map_err(|_| Error::Authentication)
}
fn derive(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0; 32]);
    let params = Params::new(65536, 3, 1, Some(32)).map_err(|_| Error::Authentication)?;
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, key.as_mut())
        .map_err(|_| Error::Authentication)?;
    Ok(key)
}
/// Every chunk authenticates the immutable header, its index and final flag.
/// The authenticated total length rejects truncation, reordering and appended data.
pub fn recovery_encrypt(password: &str, plain: &[u8]) -> Result<Vec<u8>> {
    if password.chars().count() < 8 {
        return Err(Error::Invalid("恢复口令至少 8 个字符".into()));
    }
    if plain.len() > MAX_BYTES {
        return Err(Error::Invalid("恢复包超过 512 MB 限制".into()));
    }
    let mut salt = [0; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let key = derive(password, &salt)?;
    let mut header = MAGIC.to_vec();
    header.extend(salt);
    header.extend((plain.len() as u64).to_le_bytes());
    let mut output = header.clone();
    let count = plain.len().div_ceil(CHUNK).max(1);
    for i in 0..count {
        let mut aad = header.clone();
        aad.extend((i as u64).to_le_bytes());
        aad.push(u8::from(i + 1 == count));
        let start = i * CHUNK;
        let end = (start + CHUNK).min(plain.len());
        output.extend(seal(&key, &plain[start..end], &aad)?);
    }
    Ok(output)
}
pub fn recovery_decrypt(password: &str, data: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if data.len() < 72 || &data[..8] != MAGIC {
        return Err(Error::Authentication);
    }
    let len = u64::from_le_bytes(data[24..32].try_into().map_err(|_| Error::Authentication)?);
    if len > MAX_BYTES as u64 {
        return Err(Error::Authentication);
    }
    let len = len as usize;
    let count = len.div_ceil(CHUNK).max(1);
    if data.len() != 32 + len + 40 * count {
        return Err(Error::Authentication);
    }
    let key = derive(password, &data[8..24])?;
    let mut result = Zeroizing::new(Vec::with_capacity(len));
    let mut offset = 32;
    for i in 0..count {
        let size = (len - i * CHUNK).min(CHUNK) + 40;
        let mut aad = data[..32].to_vec();
        aad.extend((i as u64).to_le_bytes());
        aad.push(u8::from(i + 1 == count));
        let plain = Zeroizing::new(open(&key, &data[offset..offset + size], &aad)?);
        result.extend_from_slice(&plain);
        offset += size;
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_authenticates_every_chunk_and_length() {
        let plain = vec![42; CHUNK + 17];
        let data = recovery_encrypt("测试口令abcdefgh", &plain).unwrap();
        assert_eq!(*recovery_decrypt("测试口令abcdefgh", &data).unwrap(), plain);
        assert!(recovery_decrypt("wrong password", &data).is_err());
        assert!(recovery_decrypt("测试口令abcdefgh", &data[..data.len() - 1]).is_err());
        let mut tampered = data;
        tampered[CHUNK + 40] ^= 1;
        assert!(recovery_decrypt("测试口令abcdefgh", &tampered).is_err());
    }
}
