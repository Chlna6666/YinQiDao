use serde_json::{Value, json};

use crate::{
    api, protocol,
    bindings::yinqidao::music_plugin::{host, types},
};

const PROVIDER_ID: &str = "netease";
const COOKIE_CHALLENGE_ID: &str = "cookie-import-v1";
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
        _ => Err("网易云插件当前支持 QR 扫码登录和 Cookie 导入登录".into()),
    }
}

pub fn auth_poll(provider_id: &str, challenge_id: &str) -> Result<types::AuthPoll, String> {
    ensure_provider(provider_id)?;
    if challenge_id == COOKIE_CHALLENGE_ID {
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
                .or_else(|| response.pointer("/data/cookie").and_then(Value::as_str))
                .filter(|cookie| !cookie.trim().is_empty())
                .ok_or_else(|| "网易云 QR 登录已授权但响应缺少 Cookie".to_string())?;
            api::auth_submit(
                provider_id,
                COOKIE_CHALLENGE_ID,
                vec![types::KeyValue {
                    key: "cookie".into(),
                    value: cookie.into(),
                }],
            )
        }
        value => Ok(types::AuthPoll::Denied(format!(
            "网易云 QR 登录返回未知状态 code={value}"
        ))),
    }
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
    let response = protocol::eapi_anonymous(
        "/api/login/qrcode/unikey",
        json!({ "type": 3 }),
    )?;
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
    }
}
