#![allow(dead_code)]

//! QQ 音乐 Web 协议所需的轻量算法：g_tk 哈希、Base64 解码、设备 guid 派生。
//!
//! 第一版插件刻意不引入任何加解密依赖：搜索/播放/歌单/歌词等只读与常规写通道均为明文
//! musicu.fcg / 老 fcgi；红心写要求的 zzc+ag-1 加密通道留待后续版本（届时以独立模块实现）。

/// DJB2/hash33 —— QQ 系接口通用字符串哈希（g_tk、ptqrtoken 均由此派生）。
pub fn hash33(value: &str, mut seed: u32) -> u32 {
    for ch in value.chars() {
        seed = (seed << 5).wrapping_add(seed).wrapping_add(ch as u32);
    }
    seed & 0x7FFF_FFFF
}

/// 登录态 g_tk：由 qm_keyst 派生；游客固定 5381。
pub fn g_tk(qm_keyst: Option<&str>) -> u32 {
    match qm_keyst.filter(|key| !key.trim().is_empty()) {
        Some(key) => hash33(key.trim(), 5381),
        None => 5381,
    }
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 标准 Base64 解码（含 padding 容错；非法输入返回 None）。
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut lookup = [255_u8; 256];
    for (index, byte) in BASE64_ALPHABET.iter().enumerate() {
        lookup[*byte as usize] = index as u8;
    }
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            break;
        }
        let value = lookup[byte as usize];
        if value == 255 {
            return None;
        }
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xFF) as u8);
        }
    }
    Some(output)
}

/// 由 Host 时钟派生稳定的设备 guid（QQ vkey 接口要求一个会话内稳定的数字串）。
pub fn device_guid(now_ms: u64) -> String {
    let mut state = now_ms ^ 0x9E37_79B9_7F4A_7C15;
    let mut digits = String::with_capacity(10);
    for _ in 0..10 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        digits.push(char::from(b'0' + ((state >> 33) % 10) as u8));
    }
    digits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash33_matches_known_g_tk_semantics() {
        // 空 key 固定 5381
        assert_eq!(g_tk(None), 5381);
        assert_eq!(g_tk(Some("  ")), 5381);
        // hash33("x", 5381) = ((5381<<5)+5381+120) & 0x7FFFFFFF = 177693（Python 核算）
        assert_eq!(hash33("x", 5381), 177_693);
    }

    #[test]
    fn base64_decode_round_trips_ascii_and_binary() {
        assert_eq!(
            base64_decode("aGVsbG8=").as_deref(),
            Some(b"hello".as_slice())
        );
        assert_eq!(
            base64_decode("aGVsbG8h").as_deref(),
            Some(b"hello!".as_slice())
        );
        // 非法字符
        assert_eq!(base64_decode("a*b="), None);
    }

    #[test]
    fn device_guid_is_ten_digits_and_stable_per_clock() {
        let guid = device_guid(1_700_000_000_000);
        assert_eq!(guid.len(), 10);
        assert!(guid.bytes().all(|byte| byte.is_ascii_digit()));
        assert_eq!(guid, device_guid(1_700_000_000_000));
    }
}
