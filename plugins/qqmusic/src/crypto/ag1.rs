//! QQ Music web zzc signature and ag-1 AES-GCM transport.
//! Constants adapted from Jixun Wu's MIT-licensed qmweb-sign
//! (Copyright (c) 2024 Jixun Wu, https://github.com/jixunmoe/qmweb-sign).
//! Every request uses an independent cryptographically random 12-byte nonce.

use aes_gcm::{
    Aes128Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use sha1::{Digest, Sha1};

const REQUEST_KEY: [u8; 16] = [
    189, 48, 95, 16, 208, 255, 116, 182, 239, 84, 218, 184, 53, 181, 225, 207,
];
const RESPONSE_KEY: [u8; 21] = [
    122, 63, 140, 29, 94, 155, 47, 10, 108, 77, 126, 139, 31, 58, 92, 157, 14, 43, 111,
    74, 129,
];
const PART_1: [usize; 8] = [23, 14, 6, 36, 16, 40, 7, 19];
const PART_2: [usize; 8] = [16, 1, 32, 12, 19, 27, 8, 5];
const SCRAMBLE: [u8; 20] = [
    89, 39, 179, 150, 218, 82, 58, 252, 177, 52, 186, 123, 120, 64, 242, 133, 143, 161,
    121, 179,
];

/// The signature MUST cover the exact bytes sent through AES-GCM.
pub(crate) fn zzc_sign(payload: &[u8]) -> String {
    let hash = Sha1::digest(payload);
    let mut upper = String::with_capacity(40);
    for byte in hash.iter() {
        use std::fmt::Write as _;
        write!(&mut upper, "{byte:02X}").expect("String write");
    }
    let first: String = PART_1.iter().filter_map(|&i| upper.chars().nth(i)).collect();
    let second: String = PART_2.iter().filter_map(|&i| upper.chars().nth(i)).collect();
    let mut scrambled = [0u8; 20];
    for (i, out) in scrambled.iter_mut().enumerate() {
        *out = SCRAMBLE[i] ^ hash[i];
    }
    let middle: String = super::base64_encode(&scrambled)
        .chars()
        .filter(|&c| !matches!(c, '\\' | '/' | '+' | '='))
        .collect();
    format!("zzc{first}{middle}{second}").to_ascii_lowercase()
}

fn encrypt_with_nonce(payload: &[u8], nonce: [u8; 12]) -> Result<Vec<u8>, String> {
    let cipher = Aes128Gcm::new_from_slice(&REQUEST_KEY)
        .map_err(|_| "QQ ag-1 AES 初始化失败".to_owned())?;
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), payload)
        .map_err(|_| "QQ ag-1 AES-GCM 加密失败".to_owned())?;
    let mut output = Vec::with_capacity(12 + encrypted.len());
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&encrypted);
    Ok(output)
}

/// Wire format: nonce (12 bytes) || ciphertext || GCM authentication tag (16).
pub(crate) fn encode_request(payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|_| "QQ ag-1 无法获取安全随机数".to_owned())?;
    encrypt_with_nonce(payload, nonce)
}

pub(crate) fn decode_response(bytes: &[u8]) -> Result<serde_json::Value, String> {
    if bytes.is_empty() {
        return Err("QQ ag-1 收到空响应".into());
    }
    let decoded: Vec<_> = bytes
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ RESPONSE_KEY[i % RESPONSE_KEY.len()])
        .collect();
    // Match the non-fatal TextDecoder used in the original web client.
    // The decrypted JSON structure is still parsed and validated strictly.
    serde_json::from_str(&String::from_utf8_lossy(&decoded))
        .map_err(|_| "QQ ag-1 响应不是合法 JSON".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zzc_upstream_vectors() {
        assert_eq!(zzc_sign(b"123"), "zzcec1b555gzqzg7laztguyjl2bu20r6x1w50c55f60");
        assert_eq!(zzc_sign(b"hello world"), "zzcfb3415bc4nfoxmd9uik71mkomtubjfjp141a1cbbcc");
        assert_eq!(zzc_sign(b"jixun.uk"), "zzcf47b78apso27mjjbbzgbof0szikfkvyqc7fc3a2b5");
    }

    #[test]
    fn aes_gcm_upstream_fixture() {
        let bytes = super::super::base64_decode(
            "UVKyyaPxYeBs+e6kkEeQpwwnsRmO4NI5JTdJvqrvU3w3elv4W1kcb8G35S4="
        ).unwrap();
        let cipher = Aes128Gcm::new_from_slice(&REQUEST_KEY).unwrap();
        let decoded = cipher.decrypt(Nonce::from_slice(&bytes[..12]), &bytes[12..]).unwrap();
        assert_eq!(decoded, b"{\"hello\":\"ag1!\"}");
        let nonce: [u8; 12] = bytes[..12].try_into().unwrap();
        assert_eq!(encrypt_with_nonce(&decoded, nonce).unwrap(), bytes);
    }

    #[test]
    fn ag1_response_upstream_fixture() {
        let bytes = super::super::base64_decode(
            "AR3vcjr+DTBcYVz/bBhmrSIJHD7gCEvTaS25FTtcfVKpa0g9/mtCC2i7WFnqezj9SWwKKxjteVw6+2gJEg=="
        ).unwrap();
        assert_eq!(
            decode_response(&bytes).unwrap(),
            serde_json::json!({"code":0,"ts":0,"start_ts":100,"traceid":"ffffffffffffffff"})
        );
        assert!(decode_response(&[]).is_err());
        assert!(decode_response(&[0xff]).is_err());
    }
}
