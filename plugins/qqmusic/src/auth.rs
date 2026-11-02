//! QQ Music authentication: Cookie import and QQ ptlogin QR OAuth.
//!
//! QQ Music mobile-app QR uses an independent MQTT-over-WebSocket protocol and
//! is intentionally not advertised until the sandbox Host exposes a secure
//! session-scoped transport for it. Never present a QR that cannot authenticate.
use serde_json::{Value, json};

use crate::api;
use crate::bindings::yinqidao::music_plugin::{host, types};
use crate::{crypto, protocol};

const PREFIX: &str = "qq-qr:";
const QR_TTL_MS: u64 = 180_000;
const MAX_QR_IMAGE_BYTES: usize = 256 * 1024;
const MAX_AUTH_HTTP_BYTES: usize = 512 * 1024;
const QQ_REFERER: &str = "https://xui.ptlogin2.qq.com/";

fn header(key: &str, value: impl Into<String>) -> types::KeyValue {
    types::KeyValue { key: key.into(), value: value.into() }
}

fn request(method: &str, url: &str, headers: Vec<types::KeyValue>, body: Vec<u8>) -> Result<host::HttpResponse, String> {
    let response = host::http_request(&host::HttpRequestData {
        provider_id: protocol::PROVIDER_ID.into(),
        account_id: None,
        method: method.into(),
        url: url.into(),
        headers,
        body,
    }).map_err(|error| format!("QQ 授权请求失败: {error}"))?;
    if !(200..400).contains(&response.status) {
        return Err(format!("QQ 授权 HTTP 状态异常: {}", response.status));
    }
    if response.body.len() > MAX_AUTH_HTTP_BYTES {
        return Err("QQ 授权响应超过安全大小限制".into());
    }
    Ok(response)
}

fn response_header<'a>(response: &'a host::HttpResponse, key: &str) -> Option<&'a str> {
    response.headers.iter()
        .find(|entry| entry.key.eq_ignore_ascii_case(key))
        .map(|entry| entry.value.as_str())
}

fn cookie_from_headers(response: &host::HttpResponse, name: &str) -> Option<String> {
    for field in &response.headers {
        if !field.key.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        // A set-cookie header has one initial name=value pair. Never include
        // response cookie attributes (path, expires, secure) in Cookie requests.
        let Some(pair) = field.value.split(';').next() else { continue };
        let Some((key, value)) = pair.trim().split_once('=') else { continue };
        if key.trim().eq_ignore_ascii_case(name) && !value.is_empty() {
            return Some(value.trim().to_string());
        }
    }
    None
}

fn cookie_header(response: &host::HttpResponse) -> String {
    response.headers.iter()
        .filter(|entry| entry.key.eq_ignore_ascii_case("set-cookie"))
        .filter_map(|entry| entry.value.split(';').next())
        .filter(|pair| pair.split_once('=').is_some())
        .collect::<Vec<_>>()
        .join("; ")
}

fn validate_qrsig(challenge_id: &str) -> Result<&str, String> {
    let qrsig = challenge_id.strip_prefix(PREFIX).ok_or("未知的 QQ 扫码 challenge")?;
    if qrsig.is_empty() || qrsig.len() > 256
        || !qrsig.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("QQ 扫码 challenge 无效".into());
    }
    Ok(qrsig)
}

fn percent_encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(char::from(byte));
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

fn query_parameter(url: &str, key: &str) -> Option<String> {
    let raw = url.split_once('?')?.1;
    raw.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key && !value.is_empty()).then(|| value.to_string())
    })
}

fn begin_qq_qr() -> Result<types::AuthChallenge, String> {
    let url = format!(
        "https://ssl.ptlogin2.qq.com/ptqrshow?appid=716027609&e=2&l=M&s=3&d=72&v=4&t={}&daid=383&pt_3rd_aid=100497308",
        host::now_ms()
    );
    let response = request("GET", &url, vec![header("Referer", QQ_REFERER)], Vec::new())?;
    let qrsig = cookie_from_headers(&response, "qrsig")
        .ok_or("QQ 扫码服务未返回 qrsig")?;
    validate_qrsig(&format!("{PREFIX}{qrsig}"))?;
    if response.body.len() > MAX_QR_IMAGE_BYTES || !response.body.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("QQ 扫码服务未返回有效的 PNG 图片".into());
    }
    Ok(types::AuthChallenge {
        challenge_id: format!("{PREFIX}{qrsig}"),
        kind: types::AuthChallengeKind::QrCode,
        verification_uri: Some("https://y.qq.com/".into()),
        user_code: None,
        qr_payload: Some(format!("data:image/png;base64,{}", crypto::base64_encode(&response.body))),
        fields: Vec::new(),
        expires_at_ms: Some(host::now_ms().saturating_add(QR_TTL_MS)),
    })
}

fn authorize_qq(uin: &str, sigx: &str) -> Result<types::Account, String> {
    if uin.is_empty() || uin.len() > 20 || !uin.bytes().all(|b| b.is_ascii_digit()) {
        return Err("QQ 扫码返回无效 UIN".into());
    }
    let url = format!(
        "https://ssl.ptlogin2.graph.qq.com/check_sig?uin={uin}&pttype=1&service=ptqrlogin&nodirect=0&ptsigx={}&s_url=https%3A%2F%2Fgraph.qq.com%2Foauth2.0%2Flogin_jump&ptlang=2052&ptredirect=100&aid=716027609&daid=383&j_later=0&low_login_hour=0&regmaster=0&pt_login_type=3&pt_aid=0&pt_aaid=16&pt_light=0&pt_3rd_aid=100497308",
        percent_encode(sigx)
    );
    // Intercept the first 302, otherwise Host auto-redirecting loses the
    // p_skey issued by check_sig before the OAuth form is submitted.
    let response = request("GET", &url, vec![
        header("Referer", QQ_REFERER),
        header("X-YinQiDao-Redirect-Mode", "manual"),
    ], Vec::new())?;
    let pskey = cookie_from_headers(&response, "p_skey")
        .ok_or("QQ OAuth 校验未返回 p_skey")?;
    let cookies = cookie_header(&response);
    let redirect_uri = "https://y.qq.com/portal/wx_redirect.html?login_type=1&surl=https://y.qq.com/";
    let auth_time = host::now_ms();
    let ui = format!("00000000-0000-4000-8000-{auth_time:012x}");
    let fields = [
        ("response_type", "code"),
        ("client_id", "100497308"),
        ("redirect_uri", redirect_uri),
        ("scope", "get_user_info,get_app_friends"),
        ("state", "state"),
        ("switch", ""),
        ("from_ptlogin", "1"),
        ("src", "1"),
        ("update_auth", "1"),
        ("openapi", "1010_1030"),
    ];
    let mut body = fields.iter()
        .map(|(key, value)| format!("{key}={}", percent_encode(value)))
        .collect::<Vec<_>>();
    body.push(format!("g_tk={}", crypto::hash33(&pskey, 5381)));
    body.push(format!("auth_time={auth_time}"));
    body.push(format!("ui={ui}"));
    let result = request("POST", "https://graph.qq.com/oauth2.0/authorize", vec![
        header("Cookie", cookies),
        header("Content-Type", "application/x-www-form-urlencoded"),
        header("Referer", "https://graph.qq.com/oauth2.0/show"),
        header("X-YinQiDao-Redirect-Mode", "manual"),
    ], body.join("&").into_bytes())?;
    let location = response_header(&result, "location")
        .ok_or("QQ OAuth 未返回授权跳转地址")?;
    if !location.starts_with("https://y.qq.com/") {
        return Err("QQ OAuth 授权跳转目标不可信".into());
    }
    let code = query_parameter(location, "code").ok_or("QQ OAuth 未返回 code")?;
    if code.len() > 512 {
        return Err("QQ OAuth code 超过大小限制".into());
    }
    let payload = json!({
        "comm": {"ct":24, "cv":4747474, "platform":"yqq.json", "tmeLoginType":2},
        "QQConnectLogin.LoginServer.QQLogin": {
            "module":"QQConnectLogin.LoginServer",
            "method":"QQLogin",
            "param":{"code":code}
        }
    });
    let exchange = request("POST", "https://u.y.qq.com/cgi-bin/musicu.fcg", vec![
        header("Referer", "https://y.qq.com/"),
        header("Origin", "https://y.qq.com"),
        header("Content-Type", "application/json"),
    ], serde_json::to_vec(&payload).map_err(|error| error.to_string())?)?;
    let data: Value = serde_json::from_slice(&exchange.body)
        .map_err(|error| format!("QQ 音乐授权响应无法解析: {error}"))?;
    let result = data.get("QQConnectLogin.LoginServer.QQLogin")
        .ok_or("QQ 音乐授权响应缺少 QQLogin 模块")?;
    let code = result.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        return Err(format!("QQ 音乐授权交换失败: code={code}"));
    }
    let credential = result.get("data").unwrap_or(result);
    let musicid = credential.get("musicid").and_then(|id| {
        id.as_u64().map(|id| id.to_string()).or_else(|| id.as_str().map(str::to_owned))
    }).ok_or("QQLogin 未返回 musicid")?;
    let musickey = credential.get("musickey")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 8192)
        .ok_or("QQLogin 未返回有效 musickey")?;
    if !musicid.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("QQLogin 返回无效 musicid".into());
    }
    // The normal cookie-import validation and Host-secret persistence path is
    // reused, so QR and Cookie login share one account/authorization model.
    let cookie = format!("uin=o{musicid}; qm_keyst={musickey}; qqmusic_key={musickey}");
    api::register_authenticated_cookie(&cookie)
}

pub fn auth_begin(provider_id: &str, method: types::AuthMethod) -> Result<types::AuthChallenge, String> {
    if provider_id != api::PROVIDER_ID {
        return Err(format!("unknown provider id: {provider_id}"));
    }
    match method {
        types::AuthMethod::CookieImport => api::auth_begin(provider_id, method),
        types::AuthMethod::QrCode => begin_qq_qr(),
        _ => Err("该认证方式尚未实现".into()),
    }
}

pub fn auth_poll(provider_id: &str, challenge_id: &str) -> Result<types::AuthPoll, String> {
    if provider_id != api::PROVIDER_ID {
        return Err(format!("unknown provider id: {provider_id}"));
    }
    if challenge_id == api::COOKIE_CHALLENGE_ID {
        return api::auth_poll(provider_id, challenge_id);
    }
    let qrsig = validate_qrsig(challenge_id)?;
    let url = format!(
        "https://ssl.ptlogin2.qq.com/ptqrlogin?u1=https%3A%2F%2Fgraph.qq.com%2Foauth2.0%2Flogin_jump&ptqrtoken={}&ptredirect=0&h=1&t=1&g=1&from_ui=1&ptlang=2052&action=0-0-{}&js_ver=20102616&js_type=1&pt_uistyle=40&aid=716027609&daid=383&pt_3rd_aid=100497308&has_onekey=1",
        crypto::hash33(qrsig, 0),
        host::now_ms()
    );
    let response = request("GET", &url, vec![
        header("Referer", QQ_REFERER),
        header("Cookie", format!("qrsig={qrsig}")),
    ], Vec::new())?;
    let body = String::from_utf8(response.body).map_err(|_| "QQ 扫码状态不是 UTF-8".to_string())?;
    if !body.starts_with("ptuiCB(") {
        return Err("QQ 扫码状态格式无效".into());
    }
    let args: Vec<&str> = body.split('\'').skip(1).step_by(2).collect();
    match args.first().copied().unwrap_or("") {
        "65" => Ok(types::AuthPoll::Expired),
        "66" | "67" => Ok(types::AuthPoll::Pending),
        "0" => {
            let url = args.get(2).copied().ok_or("QQ 扫码状态缺少授权地址")?;
            if !url.starts_with("https://") {
                return Err("QQ 扫码授权地址非法".into());
            }
            let sigx = query_parameter(url, "ptsigx").ok_or("QQ 扫码缺少 ptsigx")?;
            let uin = query_parameter(url, "uin").ok_or("QQ 扫码缺少 UIN")?;
            authorize_qq(&uin, &sigx).map(types::AuthPoll::Authenticated)
        }
        _ => Ok(types::AuthPoll::Denied("QQ 扫码已拒绝，请重新获取二维码".into())),
    }
}

pub fn auth_submit(provider_id: &str, challenge_id: &str, values: Vec<types::KeyValue>) -> Result<types::AuthPoll, String> {
    if challenge_id != api::COOKIE_CHALLENGE_ID {
        return Err("QQ 二维码认证不能提交 Cookie 表单".into());
    }
    api::auth_submit(provider_id, challenge_id, values)
}

pub fn auth_cancel(provider_id: &str, challenge_id: &str) -> Result<bool, String> {
    if provider_id != api::PROVIDER_ID {
        return Err(format!("unknown provider id: {provider_id}"));
    }
    if challenge_id == api::COOKIE_CHALLENGE_ID {
        return api::auth_cancel(provider_id, challenge_id);
    }
    validate_qrsig(challenge_id)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptlogin_ids_are_separate_from_cookie_import() {
        assert!(validate_qrsig("qq-qr:valid_123").is_ok());
        assert!(validate_qrsig("qq-qr:bad;cookie=1").is_err());
        assert!(validate_qrsig("cookie-import-v1").is_err());
    }

    #[test]
    fn oauth_query_encoding_is_bounded_and_safe() {
        assert_eq!(percent_encode("a/b&c"), "a%2Fb%26c");
        assert_eq!(query_parameter("https://y.qq.com/?code=abc&state=1", "code"), Some("abc".into()));
    }

    #[test]
    fn oauth_reply_does_not_accept_partial_credentials() {
        assert!(query_parameter("https://evil.invalid/?uin=123", "ptsigx").is_none());
    }
}
