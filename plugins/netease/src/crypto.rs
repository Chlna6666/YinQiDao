use serde_json::Value;

const AES_BLOCK: usize = 16;
const IV: &[u8; AES_BLOCK] = b"0102030405060708";
const PRESET_KEY: &[u8; AES_BLOCK] = b"0CoJUm6Qyw8W8jud";
const EAPI_KEY: &[u8; AES_BLOCK] = b"e82ckenh8dichen8";
const BASE62: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const PUBLIC_EXPONENT: u32 = 65_537;
const PUBLIC_MODULUS_HEX: &str = concat!(
    "e0b509f6259df8642dbc35662901477df22677ec152b5ff68ace615bb7b72515",
    "2b3ab17a876aea8a5aa76d2e417629ec4ee341f56135fccf695280104e0312ec",
    "bda92557c93870114af6c9d05c4f7f0c3685b7a46bee255932575cce10b424d",
    "813cfe4875d3e82047b97ddef52741d546b8e289dc6935b3ece0462db0a22b8e7"
);

const AES_SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

const AES_RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

const MD5_SHIFT: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const MD5_K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

#[derive(Debug)]
pub struct EncryptedForm {
    pub body: Vec<u8>,
}

pub fn weapi(data: &Value, entropy: u64) -> Result<EncryptedForm, String> {
    let text =
        serde_json::to_vec(data).map_err(|error| format!("序列化 WeAPI 请求失败: {error}"))?;
    let secret = derive_secret_key(entropy, &text);
    let first = base64_encode(&aes_cbc_encrypt_pkcs7(&text, PRESET_KEY, IV));
    let params = base64_encode(&aes_cbc_encrypt_pkcs7(first.as_bytes(), &secret, IV));
    let enc_sec_key = rsa_encrypt_secret(&secret)?;
    Ok(EncryptedForm {
        body: form_encode(&[("params", params), ("encSecKey", enc_sec_key)]),
    })
}

pub fn eapi(path: &str, data: &Value) -> Result<EncryptedForm, String> {
    if !path.starts_with("/api/") {
        return Err("EAPI path 必须以 /api/ 开头".into());
    }
    let text =
        serde_json::to_string(data).map_err(|error| format!("序列化 EAPI 请求失败: {error}"))?;
    let message = format!("nobody{path}use{text}md5forencrypt");
    let digest = md5_hex(message.as_bytes());
    let payload = format!("{path}-36cd479b6b5-{text}-36cd479b6b5-{digest}");
    let params = hex_upper(&aes_ecb_encrypt_pkcs7(payload.as_bytes(), EAPI_KEY));
    Ok(EncryptedForm {
        body: form_encode(&[("params", params)]),
    })
}

pub fn md5_hex(data: &[u8]) -> String {
    hex_lower(&md5_digest(data))
}

fn derive_secret_key(entropy: u64, payload: &[u8]) -> [u8; AES_BLOCK] {
    let mut state = entropy ^ 0x9e37_79b9_7f4a_7c15;
    for &byte in payload {
        state ^= u64::from(byte);
        state = state.wrapping_mul(0x100_0000_01b3);
        state ^= state.rotate_left(17);
    }
    if state == 0 {
        state = 0xa076_1d64_78bd_642f;
    }

    let mut key = [0u8; AES_BLOCK];
    for byte in &mut key {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        *byte = BASE62[(state % BASE62.len() as u64) as usize];
    }
    key
}

fn aes_ecb_encrypt_pkcs7(plaintext: &[u8], key: &[u8; AES_BLOCK]) -> Vec<u8> {
    let expanded = aes_expand_key(key);
    let mut output = pkcs7_pad(plaintext);
    for chunk in output.chunks_exact_mut(AES_BLOCK) {
        let mut block = [0u8; AES_BLOCK];
        block.copy_from_slice(chunk);
        aes_encrypt_block_with_expanded(&mut block, &expanded);
        chunk.copy_from_slice(&block);
    }
    output
}

fn aes_cbc_encrypt_pkcs7(plaintext: &[u8], key: &[u8; AES_BLOCK], iv: &[u8; AES_BLOCK]) -> Vec<u8> {
    let expanded = aes_expand_key(key);
    let mut output = pkcs7_pad(plaintext);
    let mut previous = *iv;
    for chunk in output.chunks_exact_mut(AES_BLOCK) {
        let mut block = [0u8; AES_BLOCK];
        for index in 0..AES_BLOCK {
            block[index] = chunk[index] ^ previous[index];
        }
        aes_encrypt_block_with_expanded(&mut block, &expanded);
        chunk.copy_from_slice(&block);
        previous = block;
    }
    output
}

fn pkcs7_pad(input: &[u8]) -> Vec<u8> {
    let padding = AES_BLOCK - (input.len() % AES_BLOCK);
    let mut output = Vec::with_capacity(input.len() + padding);
    output.extend_from_slice(input);
    output.extend(std::iter::repeat_n(padding as u8, padding));
    output
}

fn aes_expand_key(key: &[u8; AES_BLOCK]) -> [u8; 176] {
    let mut expanded = [0u8; 176];
    expanded[..AES_BLOCK].copy_from_slice(key);
    let mut generated = AES_BLOCK;
    let mut rcon = 1usize;

    while generated < expanded.len() {
        let mut temp = [
            expanded[generated - 4],
            expanded[generated - 3],
            expanded[generated - 2],
            expanded[generated - 1],
        ];
        if generated % AES_BLOCK == 0 {
            temp.rotate_left(1);
            for byte in &mut temp {
                *byte = AES_SBOX[*byte as usize];
            }
            temp[0] ^= AES_RCON[rcon];
            rcon += 1;
        }
        for byte in temp {
            expanded[generated] = expanded[generated - AES_BLOCK] ^ byte;
            generated += 1;
        }
    }
    expanded
}

fn aes_encrypt_block_with_expanded(block: &mut [u8; AES_BLOCK], expanded: &[u8; 176]) {
    aes_add_round_key(block, expanded, 0);
    for round in 1..10 {
        aes_sub_bytes(block);
        aes_shift_rows(block);
        aes_mix_columns(block);
        aes_add_round_key(block, expanded, round);
    }
    aes_sub_bytes(block);
    aes_shift_rows(block);
    aes_add_round_key(block, expanded, 10);
}

fn aes_add_round_key(state: &mut [u8; AES_BLOCK], expanded: &[u8; 176], round: usize) {
    let offset = round * AES_BLOCK;
    for index in 0..AES_BLOCK {
        state[index] ^= expanded[offset + index];
    }
}

fn aes_sub_bytes(state: &mut [u8; AES_BLOCK]) {
    for byte in state {
        *byte = AES_SBOX[*byte as usize];
    }
}

fn aes_shift_rows(state: &mut [u8; AES_BLOCK]) {
    let source = *state;
    state[0] = source[0];
    state[4] = source[4];
    state[8] = source[8];
    state[12] = source[12];

    state[1] = source[5];
    state[5] = source[9];
    state[9] = source[13];
    state[13] = source[1];

    state[2] = source[10];
    state[6] = source[14];
    state[10] = source[2];
    state[14] = source[6];

    state[3] = source[15];
    state[7] = source[3];
    state[11] = source[7];
    state[15] = source[11];
}

fn aes_mix_columns(state: &mut [u8; AES_BLOCK]) {
    for column in 0..4 {
        let offset = column * 4;
        let a0 = state[offset];
        let a1 = state[offset + 1];
        let a2 = state[offset + 2];
        let a3 = state[offset + 3];
        state[offset] = gmul2(a0) ^ gmul3(a1) ^ a2 ^ a3;
        state[offset + 1] = a0 ^ gmul2(a1) ^ gmul3(a2) ^ a3;
        state[offset + 2] = a0 ^ a1 ^ gmul2(a2) ^ gmul3(a3);
        state[offset + 3] = gmul3(a0) ^ a1 ^ a2 ^ gmul2(a3);
    }
}

fn gmul2(value: u8) -> u8 {
    (value << 1) ^ if value & 0x80 != 0 { 0x1b } else { 0 }
}

fn gmul3(value: u8) -> u8 {
    gmul2(value) ^ value
}

fn md5_digest(input: &[u8]) -> [u8; 16] {
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut data = Vec::with_capacity(input.len().saturating_add(72));
    data.extend_from_slice(input);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0 = 0x6745_2301u32;
    let mut b0 = 0xefcd_ab89u32;
    let mut c0 = 0x98ba_dcfeu32;
    let mut d0 = 0x1032_5476u32;

    for chunk in data.chunks_exact(64) {
        let mut words = [0u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            let start = index * 4;
            *word = u32::from_le_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }

        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;
        for index in 0..64 {
            let (f, g) = match index {
                0..=15 => ((b & c) | ((!b) & d), index),
                16..=31 => ((d & b) | ((!d) & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let next = a
                .wrapping_add(f)
                .wrapping_add(MD5_K[index])
                .wrapping_add(words[g])
                .rotate_left(MD5_SHIFT[index])
                .wrapping_add(b);
            a = d;
            d = c;
            c = b;
            b = next;
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    digest[0..4].copy_from_slice(&a0.to_le_bytes());
    digest[4..8].copy_from_slice(&b0.to_le_bytes());
    digest[8..12].copy_from_slice(&c0.to_le_bytes());
    digest[12..16].copy_from_slice(&d0.to_le_bytes());
    digest
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct U1024([u64; 16]);

impl U1024 {
    fn zero() -> Self {
        Self([0; 16])
    }

    fn one() -> Self {
        let mut limbs = [0u64; 16];
        limbs[0] = 1;
        Self(limbs)
    }

    fn from_be_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 128 {
            return Err("RSA 整数超过 1024 bit".into());
        }
        let mut limbs = [0u64; 16];
        for (index, byte) in bytes.iter().rev().enumerate() {
            limbs[index / 8] |= u64::from(*byte) << ((index % 8) * 8);
        }
        Ok(Self(limbs))
    }

    fn to_be_bytes(self) -> [u8; 128] {
        let mut bytes = [0u8; 128];
        for (index, byte) in bytes.iter_mut().rev().enumerate() {
            *byte = ((self.0[index / 8] >> ((index % 8) * 8)) & 0xff) as u8;
        }
        bytes
    }

    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        for index in (0..16).rev() {
            match self.0[index].cmp(&other.0[index]) {
                std::cmp::Ordering::Equal => {}
                ordering => return ordering,
            }
        }
        std::cmp::Ordering::Equal
    }
}

fn add_mod(left: U1024, right: U1024, modulus: U1024) -> U1024 {
    let mut sum = [0u64; 17];
    let mut carry = 0u128;
    for index in 0..16 {
        let value = u128::from(left.0[index]) + u128::from(right.0[index]) + carry;
        sum[index] = value as u64;
        carry = value >> 64;
    }
    sum[16] = carry as u64;

    let should_subtract = sum[16] != 0 || {
        let candidate = U1024(sum[..16].try_into().expect("fixed limb slice"));
        candidate.cmp(&modulus) != std::cmp::Ordering::Less
    };
    if should_subtract {
        let mut borrow = false;
        for index in 0..16 {
            let (value, borrow_a) = sum[index].overflowing_sub(modulus.0[index]);
            let (value, borrow_b) = value.overflowing_sub(u64::from(borrow));
            sum[index] = value;
            borrow = borrow_a || borrow_b;
        }
        let (top, top_borrow) = sum[16].overflowing_sub(u64::from(borrow));
        debug_assert!(!top_borrow);
        sum[16] = top;
    }
    debug_assert_eq!(sum[16], 0);
    U1024(sum[..16].try_into().expect("fixed limb slice"))
}

fn mul_mod(left: U1024, right: U1024, modulus: U1024) -> U1024 {
    let mut result = U1024::zero();
    let mut addend = left;
    for limb in right.0 {
        let mut bits = limb;
        for _ in 0..64 {
            if bits & 1 != 0 {
                result = add_mod(result, addend, modulus);
            }
            bits >>= 1;
            addend = add_mod(addend, addend, modulus);
        }
    }
    result
}

fn pow_mod(mut base: U1024, mut exponent: u32, modulus: U1024) -> U1024 {
    let mut result = U1024::one();
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = mul_mod(result, base, modulus);
        }
        exponent >>= 1;
        if exponent != 0 {
            base = mul_mod(base, base, modulus);
        }
    }
    result
}

fn rsa_encrypt_secret(secret_key: &[u8; AES_BLOCK]) -> Result<String, String> {
    let modulus_bytes = decode_hex(PUBLIC_MODULUS_HEX)?;
    let modulus = U1024::from_be_bytes(&modulus_bytes)?;
    let mut reversed = *secret_key;
    reversed.reverse();
    let message = U1024::from_be_bytes(&reversed)?;
    if message.cmp(&modulus) != std::cmp::Ordering::Less {
        return Err("WeAPI RSA 明文超出模数".into());
    }
    let encrypted = pow_mod(message, PUBLIC_EXPONENT, modulus);
    Ok(hex_lower(&encrypted.to_be_bytes()))
}

pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut index = 0usize;
    while index + 3 <= bytes.len() {
        let value = (u32::from(bytes[index]) << 16)
            | (u32::from(bytes[index + 1]) << 8)
            | u32::from(bytes[index + 2]);
        output.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        output.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        output.push(TABLE[((value >> 6) & 0x3f) as usize] as char);
        output.push(TABLE[(value & 0x3f) as usize] as char);
        index += 3;
    }
    match bytes.len() - index {
        1 => {
            let value = u32::from(bytes[index]) << 16;
            output.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
            output.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
            output.push('=');
            output.push('=');
        }
        2 => {
            let value = (u32::from(bytes[index]) << 16) | (u32::from(bytes[index + 1]) << 8);
            output.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
            output.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
            output.push(TABLE[((value >> 6) & 0x3f) as usize] as char);
            output.push('=');
        }
        _ => {}
    }
    output
}

fn form_encode(fields: &[(&str, String)]) -> Vec<u8> {
    let mut body = String::new();
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            body.push('&');
        }
        body.push_str(key);
        body.push('=');
        body.push_str(&percent_encode(value));
    }
    body.into_bytes()
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err("hex 长度必须为偶数".into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|chunk| Ok((hex_nibble(chunk[0])? << 4) | hex_nibble(chunk[1])?))
        .collect()
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("非法 hex 字符".into()),
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    encode_hex(bytes, b"0123456789abcdef")
}

fn hex_upper(bytes: &[u8]) -> String {
    encode_hex(bytes, b"0123456789ABCDEF")
}

fn encode_hex(bytes: &[u8], alphabet: &[u8; 16]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(alphabet[(byte >> 4) as usize] as char);
        output.push(alphabet[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aes_128_matches_fips_vector() {
        let key = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let mut block = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let expanded = aes_expand_key(&key);
        aes_encrypt_block_with_expanded(&mut block, &expanded);
        assert_eq!(hex_lower(&block), "69c4e0d86a7b0430d8cdb78070b4c55a");
    }

    #[test]
    fn md5_matches_reference_vector() {
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn rsa_matches_reference_vector() {
        let encrypted = rsa_encrypt_secret(b"0123456789abcdef").expect("rsa");
        assert_eq!(
            encrypted,
            "35701388baf89fed412e11269b9c76625d095ecaf17f03fa018abe19ea2d38b949debf242ee39a71ca1f6cda71b1b86a45aa909ee27f7e78e267d34e732f0de948206c3340a788d0003372183e2f753c1f78b66ac23d134ac1fc9b993156520ea826b8aa89a962d4491b4b8d7e08738e1da9b07aa39bf4a7ef0b1c210728cd52"
        );
    }

    #[test]
    fn encrypted_forms_have_expected_shape() {
        let weapi = weapi(&json!({"id": 123}), 1).expect("weapi");
        let weapi = String::from_utf8(weapi.body).expect("utf8");
        assert!(weapi.starts_with("params="));
        assert!(weapi.contains("&encSecKey="));

        let eapi = eapi("/api/song/like", &json!({"trackId": 123})).expect("eapi");
        let eapi = String::from_utf8(eapi.body).expect("utf8");
        assert!(eapi.starts_with("params="));
    }

    #[test]
    fn base64_encode_matches_standard() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn percent_encode_matches_expected() {
        assert_eq!(percent_encode("abc-123_.~"), "abc-123_.~");
        assert_eq!(percent_encode("hello world"), "hello%20world");
        assert_eq!(percent_encode("你好"), "%E4%BD%A0%E5%A5%BD");
    }
}
