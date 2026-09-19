use serde_json::{Map, Value};

use crate::{
    bindings::yinqidao::music_plugin::{host, types},
    crypto,
};

const PROVIDER_ID: &str = "netease";
const COOKIE_KEY: &str = "cookie-v1";
const WEB_BASE: &str = "https://music.163.com";
const EAPI_BASE: &str = "https://interface.music.163.com";
const WEB_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36";
const EAPI_USER_AGENT: &str =
    "NeteaseMusic/3.1.17.204416 (Windows; Microsoft-Windows-10-Professional-build-19045-64bit)";
const MAX_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_COOKIE_BYTES: usize = 16 * 1024;

pub fn get(account_id: &str, path: &str) -> Result<Value, String> {
    validate_relative_path(path)?;
    let cookie = account_cookie(account_id)?;
    send(
        Some(account_id),
        "GET",
        &format!("{WEB_BASE}{path}"),
        &cookie,
        WEB_USER_AGENT,
        Vec::new(),
        &[],
    )
}

pub fn weapi(account_id: &str, path: &str, data: Value) -> Result<Value, String> {
    let cookie = account_cookie(account_id)?;
    weapi_with(Some(account_id), account_id, &cookie, path, data)
}

pub fn weapi_anonymous(path: &str, data: Value) -> Result<Value, String> {
    weapi_with(None, "anonymous", "", path, data)
}

fn weapi_with(
    account_id: Option<&str>,
    identity: &str,
    cookie: &str,
    path: &str,
    mut data: Value,
) -> Result<Value, String> {
    validate_api_path(path)?;
    let csrf = cookie_value(cookie, "__csrf").unwrap_or_default();
    let object = data
        .as_object_mut()
        .ok_or_else(|| "WeAPI payload 必须是 JSON object".to_string())?;
    object.insert("csrf_token".into(), Value::String(csrf.into()));

    let entropy =
        host::now_ms() ^ stable_entropy(identity.as_bytes()) ^ stable_entropy(path.as_bytes());
    let encrypted = crypto::weapi(&data, entropy)?;
    let endpoint = format!("{WEB_BASE}/weapi/{}", path.trim_start_matches("/api/"));
    send(
        account_id,
        "POST",
        &endpoint,
        cookie,
        WEB_USER_AGENT,
        encrypted.body,
        &[],
    )
}

pub fn eapi(account_id: &str, path: &str, data: Value) -> Result<Value, String> {
    let cookie = account_cookie(account_id)?;
    eapi_with(Some(account_id), account_id, &cookie, path, data, &[])
}

pub fn eapi_anonymous(path: &str, data: Value) -> Result<Value, String> {
    eapi_with(None, "anonymous", "", path, data, &[])
}

/// Anonymous EAPI request used by login flows whose business status codes are intentionally outside
/// the ordinary 2xx API range (for example QR login 800/801/802/803). HTTP transport failures are
/// still rejected; only explicitly listed JSON `code` values bypass normal response-code validation.
pub fn eapi_anonymous_status(
    path: &str,
    data: Value,
    accepted_codes: &[i64],
) -> Result<Value, String> {
    eapi_with(None, "anonymous", "", path, data, accepted_codes)
}

fn eapi_with(
    account_id: Option<&str>,
    identity: &str,
    cookie: &str,
    path: &str,
    mut data: Value,
    accepted_codes: &[i64],
) -> Result<Value, String> {
    validate_api_path(path)?;
    let now_ms = host::now_ms();
    let header = eapi_header(identity, cookie, now_ms);
    let header_cookie = eapi_cookie(&header);

    let object = data
        .as_object_mut()
        .ok_or_else(|| "EAPI payload 必须是 JSON object".to_string())?;
    object.insert("header".into(), Value::Object(header));
    object.insert("e_r".into(), Value::Bool(false));

    let encrypted = crypto::eapi(path, &data)?;
    let endpoint = format!("{EAPI_BASE}/eapi/{}", path.trim_start_matches("/api/"));
    send(
        account_id,
        "POST",
        &endpoint,
        &header_cookie,
        EAPI_USER_AGENT,
        encrypted.body,
        accepted_codes,
    )
}

fn eapi_header(identity: &str, cookie: &str, now_ms: u64) -> Map<String, Value> {
    let csrf = cookie_value(cookie, "__csrf").unwrap_or_default();
    let music_u = cookie_value(cookie, "MUSIC_U");
    let music_a = cookie_value(cookie, "MUSIC_A");
    let device_id = crypto::md5_hex(format!("yinqidao-netease-device:{identity}").as_bytes());
    let request_suffix = stable_entropy(format!("{identity}:{now_ms}").as_bytes()) % 10_000;

    let mut header = Map::new();
    header.insert(
        "osver".into(),
        Value::String("Microsoft-Windows-10-Professional-build-19045-64bit".into()),
    );
    header.insert("deviceId".into(), Value::String(device_id));
    header.insert("os".into(), Value::String("pc".into()));
    header.insert("appver".into(), Value::String("3.1.17.204416".into()));
    header.insert("versioncode".into(), Value::String("140".into()));
    header.insert("mobilename".into(), Value::String(String::new()));
    header.insert(
        "buildver".into(),
        Value::String((now_ms / 1_000).to_string()),
    );
    header.insert("resolution".into(), Value::String("1920x1080".into()));
    header.insert("__csrf".into(), Value::String(csrf.into()));
    header.insert("channel".into(), Value::String("netease".into()));
    header.insert(
        "requestId".into(),
        Value::String(format!("{now_ms}_{request_suffix:04}")),
    );
    if let Some(value) = music_u {
        header.insert("MUSIC_U".into(), Value::String(value.into()));
    }
    if let Some(value) = music_a {
        header.insert("MUSIC_A".into(), Value::String(value.into()));
    }
    header
}

fn eapi_cookie(header: &Map<String, Value>) -> String {
    let mut names = header.keys().map(String::as_str).collect::<Vec<_>>();
    names.sort_unstable();
    let mut cookie = String::new();
    for name in names {
        let Some(value) = header.get(name).and_then(Value::as_str) else {
            continue;
        };
        if !cookie.is_empty() {
            cookie.push_str("; ");
        }
        cookie.push_str(&percent_encode(name));
        cookie.push('=');
        cookie.push_str(&percent_encode(value));
    }
    cookie
}

fn send(
    account_id: Option<&str>,
    method: &str,
    url: &str,
    cookie: &str,
    user_agent: &str,
    body: Vec<u8>,
    accepted_codes: &[i64],
) -> Result<Value, String> {
    if let Some(account_id) = account_id {
        validate_account_id(account_id)?;
    }
    if !matches!(method, "GET" | "POST") {
        return Err("网易云协议层拒绝未知 HTTP method".into());
    }
    if body.len() > MAX_REQUEST_BODY_BYTES {
        return Err(format!(
            "网易云协议请求体超过插件上限 {MAX_REQUEST_BODY_BYTES} bytes"
        ));
    }

    let mut headers = vec![
        types::KeyValue {
            key: "User-Agent".into(),
            value: user_agent.into(),
        },
        types::KeyValue {
            key: "Accept".into(),
            value: "application/json, text/plain, */*".into(),
        },
        types::KeyValue {
            key: "Referer".into(),
            value: "https://music.163.com/".into(),
        },
        types::KeyValue {
            key: "Cookie".into(),
            value: ensure_os_cookie(cookie),
        },
    ];
    if method == "POST" {
        headers.push(types::KeyValue {
            key: "Content-Type".into(),
            value: "application/x-www-form-urlencoded; charset=UTF-8".into(),
        });
        headers.push(types::KeyValue {
            key: "Origin".into(),
            value: "https://music.163.com".into(),
        });
    }

    let response = host::http_request(&host::HttpRequestData {
        provider_id: PROVIDER_ID.into(),
        account_id: account_id.map(str::to_owned),
        method: method.into(),
        url: url.into(),
        headers,
        body,
    })
    .map_err(|error| format!("网易云 Host HTTP 调用失败: {error}"))?;

    if response.body.len() > MAX_HTTP_BODY_BYTES {
        return Err(format!(
            "网易云 HTTP 响应超过插件上限 {MAX_HTTP_BODY_BYTES} bytes"
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(format!("网易云 HTTP 状态异常: {}", response.status));
    }

    let mut value: Value = serde_json::from_slice(&response.body)
        .map_err(|error| format!("网易云返回 JSON 解析失败: {error}"))?;
    validate_response_code(&value, accepted_codes)?;

    // Extract cookies from Set-Cookie headers and merge into JSON response
    let header_cookies = extract_cookies_from_headers(&response.headers);
    if let Value::Object(ref mut map) = value {
        let body_cookie = map.get("cookie").and_then(Value::as_str).unwrap_or("").trim();
        let combined = if !header_cookies.is_empty() && !body_cookie.is_empty() {
            merge_cookie_strings(body_cookie, &header_cookies)
        } else if !header_cookies.is_empty() {
            header_cookies
        } else {
            body_cookie.to_string()
        };
        if !combined.is_empty() {
            map.insert("cookie".into(), Value::String(combined));
        }
    }

    Ok(value)
}

pub fn extract_cookies_from_headers(headers: &[types::KeyValue]) -> String {
    let mut map = std::collections::BTreeMap::new();
    for header in headers {
        if header.key.eq_ignore_ascii_case("set-cookie") {
            let pair = header.value.split(';').next().unwrap_or("").trim();
            if let Some((k, v)) = pair.split_once('=') {
                let k = k.trim();
                let v = v.trim();
                if !k.is_empty() && !v.is_empty() {
                    let k_lower = k.to_ascii_lowercase();
                    if !matches!(
                        k_lower.as_str(),
                        "path"
                            | "domain"
                            | "expires"
                            | "max-age"
                            | "samesite"
                            | "priority"
                            | "httponly"
                            | "secure"
                    ) {
                        map.insert(k.to_string(), v.to_string());
                    }
                }
            }
        }
    }
    map.into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn merge_cookie_strings(c1: &str, c2: &str) -> String {
    let mut map = std::collections::BTreeMap::new();
    for c in [c1, c2] {
        for part in c.split(';') {
            let part = part.trim();
            if let Some((k, v)) = part.split_once('=') {
                let k = k.trim();
                let v = v.trim();
                if !k.is_empty() && !v.is_empty() {
                    map.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    map.into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn http_get(url: &str, headers: Vec<types::KeyValue>) -> Result<Vec<u8>, String> {
    let response = host::http_request(&host::HttpRequestData {
        provider_id: PROVIDER_ID.into(),
        account_id: None,
        method: "GET".into(),
        url: url.into(),
        headers,
        body: Vec::new(),
    })
    .map_err(|error| format!("Host HTTP 调用失败: {error}"))?;

    if !(200..300).contains(&response.status) {
        return Err(format!("HTTP 状态异常: {}", response.status));
    }
    if response.body.len() > MAX_HTTP_BODY_BYTES {
        return Err("HTTP 响应超过上限".into());
    }
    Ok(response.body)
}

fn account_cookie(account_id: &str) -> Result<String, String> {
    validate_account_id(account_id)?;
    let scope = host::SecretScope {
        provider_id: PROVIDER_ID.into(),
        account_id: Some(account_id.into()),
    };
    let bytes = host::secret_get(&scope, COOKIE_KEY)
        .map_err(|error| format!("Host Secret 读取失败: {error}"))?
        .ok_or_else(|| "网易云账号 Cookie 不存在，请重新登录".to_string())?;
    if bytes.len() > MAX_COOKIE_BYTES {
        return Err("网易云 Cookie 超过大小限制".into());
    }
    let cookie =
        String::from_utf8(bytes).map_err(|_| "网易云 Cookie 不是合法 UTF-8".to_string())?;
    if cookie.contains('\r') || cookie.contains('\n') || cookie.contains('\0') {
        return Err("网易云 Cookie 包含非法控制字符".into());
    }
    Ok(cookie)
}

fn ensure_os_cookie(cookie: &str) -> String {
    if cookie.split(';').any(|part| part.trim().starts_with("os=")) {
        cookie.to_owned()
    } else if cookie.is_empty() {
        "os=pc".into()
    } else {
        format!("{cookie}; os=pc")
    }
}

fn cookie_value<'a>(cookie: &'a str, key: &str) -> Option<&'a str> {
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == key).then_some(value.trim())
    })
}

fn validate_api_path(path: &str) -> Result<(), String> {
    if !path.starts_with("/api/")
        || path.contains('\r')
        || path.contains('\n')
        || path.contains('\0')
        || path.contains("..")
        || path.contains('?')
        || path.contains('#')
    {
        return Err("网易云 API path 非法".into());
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('\r')
        || path.contains('\n')
        || path.contains('\0')
        || path.contains("..")
    {
        return Err("网易云相对 API path 非法".into());
    }
    Ok(())
}

fn validate_account_id(account_id: &str) -> Result<(), String> {
    if account_id.is_empty()
        || account_id.len() > 32
        || !account_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("网易云 account id 非法".into());
    }
    Ok(())
}

fn validate_response_code(value: &Value, accepted_codes: &[i64]) -> Result<(), String> {
    if let Some(code) = value.get("code").and_then(Value::as_i64)
        && !(200..300).contains(&code)
        && !accepted_codes.contains(&code)
    {
        let message = value
            .get("message")
            .or_else(|| value.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("网易云 API 错误 code={code}: {message}"));
    }
    Ok(())
}

fn stable_entropy(bytes: &[u8]) -> u64 {
    let mut value = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        value ^= u64::from(byte);
        value = value.wrapping_mul(0x100_0000_01b3);
    }
    value
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_parser_matches_exact_name() {
        let cookie = "MUSIC_U=token; __csrf=csrf-value; os=pc";
        assert_eq!(cookie_value(cookie, "MUSIC_U"), Some("token"));
        assert_eq!(cookie_value(cookie, "__csrf"), Some("csrf-value"));
        assert_eq!(cookie_value(cookie, "MUSIC"), None);
    }

    #[test]
    fn path_validation_separates_encrypted_and_plain_routes() {
        assert!(validate_api_path("/api/song/like").is_ok());
        assert!(validate_api_path("/api/../secret").is_err());
        assert!(validate_api_path("/api/song/like?id=1").is_err());
        assert!(validate_relative_path("/api/discovery/simiSong?songid=1").is_ok());
        assert!(validate_relative_path("//evil.example/path").is_err());
    }

    #[test]
    fn eapi_header_keeps_login_token_in_protocol_boundary() {
        let header = eapi_header("123", "MUSIC_U=token; __csrf=csrf-value", 1_700_000_000_123);
        assert_eq!(header.get("MUSIC_U").and_then(Value::as_str), Some("token"));
        assert_eq!(
            header.get("__csrf").and_then(Value::as_str),
            Some("csrf-value")
        );
        assert_eq!(header.get("os").and_then(Value::as_str), Some("pc"));
    }

    #[test]
    fn login_status_codes_are_opt_in() {
        let response = serde_json::json!({ "code": 803 });
        assert!(validate_response_code(&response, &[]).is_err());
        assert!(validate_response_code(&response, &[800, 801, 802, 803]).is_ok());
    }
}
