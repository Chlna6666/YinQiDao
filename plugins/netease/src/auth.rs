use serde_json::{Value, json};

use crate::{
    api,
    bindings::yinqidao::music_plugin::{host, types},
    crypto, protocol,
};

const PROVIDER_ID: &str = "netease";
const COOKIE_CHALLENGE_ID: &str = "cookie-import-v1";
const FORM_CHALLENGE_ID: &str = "phone-login-v1";
const ANON_CHALLENGE_ID: &str = "anonymous-login-v1";
const QR_CHALLENGE_PREFIX: &str = "qr:";
const QR_EXPIRES_AFTER_MS: u64 = 5 * 60 * 1_000;
const MAX_QR_KEY_BYTES: usize = 512;

pub fn auth_begin(
    provider_id: &str,
    method: types::AuthMethod,
) -> Result<types::AuthChallenge, String> {
    ensure_provider(provider_id)?;
    match method {
        types::AuthMethod::CookieImport => {
            api::auth_begin(provider_id, types::AuthMethod::CookieImport)
        }
        types::AuthMethod::QrCode => begin_qr(),
        types::AuthMethod::CustomForm => begin_form(),
        _ => Err("网易云插件当前支持 QR 扫码登录、Cookie 导入与手机号/验证码登录".into()),
    }
}

pub fn auth_poll(provider_id: &str, challenge_id: &str) -> Result<types::AuthPoll, String> {
    ensure_provider(provider_id)?;
    if challenge_id == COOKIE_CHALLENGE_ID
        || challenge_id == FORM_CHALLENGE_ID
        || challenge_id == ANON_CHALLENGE_ID
    {
        return api::auth_poll(provider_id, challenge_id);
    }

    let key = qr_key(challenge_id)?;
    let response = protocol::eapi_anonymous_status(
        "/api/login/qrcode/client/login",
        json!({
            "key": key,
            "type": 3,
        }),
        &[800, 801, 802, 803],
    )?;
    let code = response
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| "网易云 QR 登录状态缺少 code".to_string())?;

    match code {
        800 => Ok(types::AuthPoll::Expired),
        801 | 802 => Ok(types::AuthPoll::Pending),
        803 => {
            let cookie = response
                .get("cookie")
                .and_then(Value::as_str)
                .or_else(|| response.get("cookies").and_then(Value::as_str))
                .or_else(|| response.pointer("/data/cookie").and_then(Value::as_str))
                .or_else(|| response.pointer("/data/cookies").and_then(Value::as_str))
                .filter(|cookie| !cookie.trim().is_empty())
                .ok_or_else(|| "网易云 QR 登录已授权但响应缺少 Cookie".to_string())?;
            let account = api::register_authenticated_cookie(cookie)?;
            Ok(types::AuthPoll::Authenticated(account))
        }
        value => Ok(types::AuthPoll::Denied(format!(
            "网易云 QR 登录返回未知状态 code={value}"
        ))),
    }
}

pub fn auth_submit(
    provider_id: &str,
    challenge_id: &str,
    values: Vec<types::KeyValue>,
) -> Result<types::AuthPoll, String> {
    ensure_provider(provider_id)?;
    if challenge_id == COOKIE_CHALLENGE_ID {
        return api::auth_submit(provider_id, challenge_id, values);
    }
    if challenge_id == FORM_CHALLENGE_ID || challenge_id == ANON_CHALLENGE_ID {
        let is_anon = values
            .iter()
            .find(|v| v.key == "anonymous")
            .map(|v| v.value.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(challenge_id == ANON_CHALLENGE_ID);

        if is_anon {
            let now = host::now_ms();
            let device_id = crypto::md5_hex(format!("yinqidao-anon:{now}").as_bytes());
            let username = crypto::base64_encode(device_id.as_bytes());
            let response = protocol::weapi_anonymous(
                "/api/register/anonimous",
                json!({ "username": username }),
            )?;
            let user_id = response
                .get("userId")
                .and_then(Value::as_u64)
                .ok_or_else(|| "网易云匿名登录响应缺少 userId".to_string())?;
            let cookie = response
                .get("cookie")
                .and_then(Value::as_str)
                .ok_or_else(|| "网易云匿名登录响应缺少 cookie".to_string())?;
            let account = api::register_anonymous_account(user_id, cookie)?;
            return Ok(types::AuthPoll::Authenticated(account));
        }

        let phone = values
            .iter()
            .find(|v| v.key == "phone")
            .map(|v| v.value.trim())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| "登录表单缺少手机号".to_string())?;

        let captcha = values
            .iter()
            .find(|v| v.key == "captcha")
            .map(|v| v.value.trim())
            .filter(|v| !v.is_empty());

        let password = values
            .iter()
            .find(|v| v.key == "password")
            .map(|v| v.value.trim())
            .filter(|v| !v.is_empty());

        let response = if let Some(captcha) = captcha {
            protocol::weapi_anonymous(
                "/api/login/cellphone",
                json!({
                    "phone": phone,
                    "countrycode": "86",
                    "captcha": captcha,
                    "rememberLogin": "true",
                }),
            )?
        } else if let Some(password) = password {
            let md5_pass =
                if password.len() == 32 && password.chars().all(|c| c.is_ascii_hexdigit()) {
                    password.to_lowercase()
                } else {
                    crypto::md5_hex(password.as_bytes())
                };
            protocol::weapi_anonymous(
                "/api/login/cellphone",
                json!({
                    "phone": phone,
                    "countrycode": "86",
                    "password": md5_pass,
                    "rememberLogin": "true",
                }),
            )?
        } else {
            return Err("手机号登录需要提供密码或短信验证码".into());
        };

        let cookie = response
            .get("cookie")
            .and_then(Value::as_str)
            .or_else(|| response.pointer("/data/cookie").and_then(Value::as_str))
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| {
                response
                    .get("msg")
                    .and_then(Value::as_str)
                    .or_else(|| response.get("message").and_then(Value::as_str))
                    .unwrap_or("网易云手机登录失败")
                    .to_string()
            })?;

        let account = api::register_authenticated_cookie(cookie)?;
        return Ok(types::AuthPoll::Authenticated(account));
    }

    Err(format!("未知登录 challenge: {challenge_id}"))
}

#[allow(dead_code)]
pub fn send_sms_captcha(phone: &str) -> Result<bool, String> {
    let phone = phone.trim();
    if phone.len() != 11 || !phone.chars().all(|c| c.is_ascii_digit()) {
        return Err("手机号格式不正确 (需11位数字)".into());
    }
    let response = protocol::weapi_anonymous(
        "/api/sms/captcha/sent",
        json!({
            "cellphone": phone,
            "ctcode": "86",
        }),
    )?;
    let code = response.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code == 200 {
        Ok(true)
    } else {
        let msg = response
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| response.get("msg").and_then(Value::as_str))
            .unwrap_or("发送短信验证码失败");
        Err(msg.to_string())
    }
}

#[allow(dead_code)]
pub fn refresh_token(account_id: &str) -> Result<bool, String> {
    let response = protocol::weapi(account_id, "/api/login/token/refresh", json!({}))?;
    let code = response.get("code").and_then(Value::as_i64).unwrap_or(0);
    Ok(code == 200)
}

#[allow(dead_code)]
pub fn check_login_status(account_id: &str) -> Result<bool, String> {
    let response = protocol::get(account_id, "/api/nuser/account/get")?;
    let profile = response.get("profile");
    Ok(profile.is_some_and(|p| !p.is_null()))
}

fn begin_form() -> Result<types::AuthChallenge, String> {
    Ok(types::AuthChallenge {
        challenge_id: FORM_CHALLENGE_ID.into(),
        kind: types::AuthChallengeKind::Form,
        verification_uri: Some("https://music.163.com/".into()),
        user_code: None,
        qr_payload: None,
        fields: vec![
            types::KeyValue {
                key: "phone".into(),
                value: "手机号 (若匿名登录可留空)".into(),
            },
            types::KeyValue {
                key: "password".into(),
                value: "密码 (留空则使用验证码)".into(),
            },
            types::KeyValue {
                key: "captcha".into(),
                value: "短信验证码 (留空则使用密码)".into(),
            },
            types::KeyValue {
                key: "anonymous".into(),
                value: "填 true 启用匿名游客登录".into(),
            },
        ],
        expires_at_ms: Some(host::now_ms().saturating_add(15 * 60 * 1_000)),
    })
}

pub fn auth_cancel(provider_id: &str, challenge_id: &str) -> Result<bool, String> {
    ensure_provider(provider_id)?;
    if challenge_id == COOKIE_CHALLENGE_ID {
        return api::auth_cancel(provider_id, challenge_id);
    }
    qr_key(challenge_id)?;
    Ok(true)
}

fn begin_qr() -> Result<types::AuthChallenge, String> {
    let response = protocol::eapi_anonymous("/api/login/qrcode/unikey", json!({ "type": 3 }))?;
    let key = response
        .get("unikey")
        .and_then(Value::as_str)
        .or_else(|| response.pointer("/data/unikey").and_then(Value::as_str))
        .ok_or_else(|| "网易云 QR key 响应缺少 unikey".to_string())?;
    validate_qr_key(key)?;

    let challenge_id = format!("{QR_CHALLENGE_PREFIX}{key}");
    let qr_payload = format!("https://music.163.com/login?codekey={key}");
    Ok(types::AuthChallenge {
        challenge_id,
        kind: types::AuthChallengeKind::QrCode,
        verification_uri: Some("https://music.163.com/".into()),
        user_code: None,
        qr_payload: Some(qr_payload),
        fields: Vec::new(),
        expires_at_ms: Some(host::now_ms().saturating_add(QR_EXPIRES_AFTER_MS)),
    })
}

fn qr_key(challenge_id: &str) -> Result<&str, String> {
    let key = challenge_id
        .strip_prefix(QR_CHALLENGE_PREFIX)
        .ok_or_else(|| "未知或已过期的网易云登录 challenge".to_string())?;
    validate_qr_key(key)?;
    Ok(key)
}

fn validate_qr_key(key: &str) -> Result<(), String> {
    if key.is_empty()
        || key.len() > MAX_QR_KEY_BYTES
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("网易云 QR login key 非法".into());
    }
    Ok(())
}

fn ensure_provider(provider_id: &str) -> Result<(), String> {
    if provider_id == PROVIDER_ID {
        Ok(())
    } else {
        Err(format!("unknown provider id: {provider_id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_challenge_id_is_bounded_and_namespaced() {
        assert_eq!(qr_key("qr:abc-123_def.0").unwrap(), "abc-123_def.0");
        assert!(qr_key("cookie-import-v1").is_err());
        assert!(qr_key("qr:../escape").is_err());
        assert!(qr_key("qr:").is_err());
    }

    #[test]
    fn qr_key_validation_rejects_special_characters() {
        assert!(validate_qr_key("normalKey123").is_ok());
        assert!(validate_qr_key("key-with_special.dots").is_ok());
        assert!(validate_qr_key("").is_err());
        assert!(validate_qr_key("key with spaces").is_err());
        assert!(validate_qr_key("key/with/slashes").is_err());
    }

    #[test]
    fn form_challenge_id_is_stable() {
        assert_eq!(FORM_CHALLENGE_ID, "phone-login-v1");
        assert_eq!(COOKIE_CHALLENGE_ID, "cookie-import-v1");
    }
}
