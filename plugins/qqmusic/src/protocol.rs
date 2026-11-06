//! QQ 音乐 Web 网关协议：musicu.fcg 模块化信封 + 老 fcgi GET 通道。
//!
//! 协议要点（2026-08~10 社区实测口径，来源见插件 README）：
//! - 主网关 `https://u.y.qq.com/cgi-bin/musicu.fcg`，必须携带 `Referer: https://y.qq.com/`
//!   （缺失返回 code -1310）。
//! - 游客态 `comm.uin = "0"`、`g_tk = 5381`；登录态 `comm.uin` 为真实 UIN、`comm.authst`
//!   为 qm_keyst，并携带 Cookie（uin / qm_keyst / euin）。
//! - 搜索模块 `music.search.SearchCgiService` 有特殊信封：请求 key 必须等于 module 名且
//!   **不带顶层 comm**，否则返回空数据。
//! - `code: 2001` 为搜索瞬时错误，立即重试一次可恢复。

use serde_json::{Map, Value};

use crate::bindings::yinqidao::music_plugin::{host, types};
use crate::crypto;

pub const PROVIDER_ID: &str = "qqmusic";
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36";
pub const MAX_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;

const MUSICU_GATEWAY: &str = "https://u.y.qq.com/cgi-bin/musicu.fcg";
const AG1_GATEWAY: &str = "https://u6.y.qq.com/cgi-bin/musics.fcg";
const YQQ_REFERER: &str = "https://y.qq.com/";

pub type ProtocolResult<T> = Result<T, String>;

/// 会话上下文：登录 Cookie（可空 = 游客）与其派生 g_tk。
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub(crate) cookie: Option<String>,
    pub(crate) uin: String,
    pub(crate) qm_keyst: Option<String>,
    pub(crate) guid: String,
}

impl Session {
    pub fn guest(now_ms: u64) -> Self {
        Self {
            cookie: None,
            uin: "0".into(),
            qm_keyst: None,
            guid: crypto::device_guid(now_ms),
        }
    }

    pub fn logged_in(cookie: String, uin: String, qm_keyst: Option<String>, now_ms: u64) -> Self {
        Self {
            cookie: Some(cookie),
            uin,
            qm_keyst,
            guid: crypto::device_guid(now_ms),
        }
    }

    fn g_tk(&self) -> u32 {
        crypto::g_tk(self.qm_keyst.as_deref())
    }
}

fn comm_block(session: &Session) -> Map<String, Value> {
    let mut comm = Map::new();
    comm.insert("ct".into(), Value::from(24));
    comm.insert("cv".into(), Value::from(4_747_474));
    comm.insert("platform".into(), Value::from("yqq.json"));
    comm.insert("uin".into(), Value::from(session.uin.clone()));
    if let Some(key) = session.qm_keyst.as_deref() {
        comm.insert("authst".into(), Value::from(key));
    }
    let g_tk = session.g_tk();
    comm.insert("g_tk".into(), Value::from(g_tk));
    comm.insert("g_tk_new_20200303".into(), Value::from(g_tk));
    comm.insert("format".into(), Value::from("json"));
    comm.insert("inCharset".into(), Value::from("utf-8"));
    comm.insert("outCharset".into(), Value::from("utf-8"));
    comm.insert("notice".into(), Value::from(0));
    comm.insert("need_new_code".into(), Value::from(1));
    comm
}

fn base_headers(session: &Session) -> Vec<types::KeyValue> {
    let mut headers = vec![
        types::KeyValue {
            key: "User-Agent".into(),
            value: USER_AGENT.into(),
        },
        types::KeyValue {
            key: "Accept".into(),
            value: "application/json, text/plain, */*".into(),
        },
        types::KeyValue {
            key: "Referer".into(),
            value: YQQ_REFERER.into(),
        },
        types::KeyValue {
            key: "Origin".into(),
            value: YQQ_REFERER.trim_end_matches('/').into(),
        },
    ];
    if let Some(cookie) = &session.cookie {
        headers.push(types::KeyValue {
            key: "Cookie".into(),
            value: cookie.clone(),
        });
    }
    headers
}

fn send(
    session: &Session,
    account_id: Option<&str>,
    url: &str,
    body: Vec<u8>,
) -> ProtocolResult<Value> {
    let mut headers = base_headers(session);
    headers.push(types::KeyValue {
        key: "Content-Type".into(),
        value: "application/json".into(),
    });
    let request = host::HttpRequestData {
        provider_id: PROVIDER_ID.into(),
        account_id: account_id.map(str::to_owned),
        method: "POST".into(),
        url: url.into(),
        headers,
        body,
    };
    let response = host::http_request(&request)
        .map_err(|error| format!("QQ音乐 Host HTTP 调用失败: {error}"))?;
    if response.body.len() > MAX_HTTP_BODY_BYTES {
        return Err(format!(
            "QQ音乐 HTTP 响应超过插件上限 {MAX_HTTP_BODY_BYTES} bytes"
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(format!("QQ音乐 HTTP 状态异常: {}", response.status));
    }
    serde_json::from_slice(&response.body)
        .map_err(|error| format!("QQ音乐返回 JSON 解析失败: {error}"))
}

/// 发送一次 musicu.fcg 模块化请求。
///
/// `modules` 的每个元素为 `(信封 key, module, method, param)`；响应按相同 key 回传。
/// `require_module_code` 决定是否校验模块块内的 `code`（搜索等模块偶发 2001 由调用方重试）。
pub fn musicu(
    session: &Session,
    account_id: Option<&str>,
    modules: &[(&str, &str, &str, Value)],
    require_module_code: bool,
) -> ProtocolResult<Value> {
    if modules.is_empty() {
        return Err("musicu 请求缺少业务模块".into());
    }
    let mut envelope = Map::new();
    envelope.insert("comm".into(), Value::Object(comm_block(session)));
    for (key, module, method, param) in modules {
        let mut block = Map::new();
        block.insert("module".into(), Value::from((*module).to_owned()));
        block.insert("method".into(), Value::from((*method).to_owned()));
        block.insert("param".into(), param.clone());
        envelope.insert((*key).to_owned(), Value::Object(block));
    }
    let body = serde_json::to_vec(&Value::Object(envelope))
        .map_err(|error| format!("musicu 请求序列化失败: {error}"))?;
    let value = send(session, account_id, MUSICU_GATEWAY, body)?;
    if let Some(code) = value.get("code").and_then(Value::as_i64)
        && !(200..300).contains(&code)
    {
        return Err(format!("QQ音乐网关错误 code={code}"));
    }
    if require_module_code {
        for (key, ..) in modules {
            if let Some(block) = value.get(*key)
                && let Some(code) = block.get("code").and_then(Value::as_i64)
                && !(200..300).contains(&code)
            {
                return Err(format!("QQ音乐模块 {key} 错误 code={code}"));
            }
        }
    }
    Ok(value)
}

/// Only use the signed and encrypted transport for a provider-owned write.
/// Never fall back to the plaintext musicu endpoint after a failure.
pub(crate) fn musics_ag1_write(
    session: &Session,
    account_id: &str,
    request_key: &str,
    module: &str,
    method: &str,
    param: Value,
    now_ms: u64,
) -> ProtocolResult<Value> {
    if session.cookie.is_none() || session.uin != account_id {
        return Err("QQ ag-1 写入需要匹配的已登录账号".into());
    }
    let mut envelope = Map::new();
    envelope.insert("comm".into(), Value::Object(comm_block(session)));
    let mut block = Map::new();
    block.insert("module".into(), Value::from(module));
    block.insert("method".into(), Value::from(method));
    block.insert("param".into(), param);
    envelope.insert(request_key.into(), Value::Object(block));
    let payload = serde_json::to_vec(&Value::Object(envelope))
        .map_err(|error| format!("序列化 QQ ag-1 写入失败: {error}"))?;
    let sign = crypto::ag1::zzc_sign(&payload);
    let body = crypto::ag1::encode_request(&payload)?;
    let mut headers = base_headers(session);
    headers.push(types::KeyValue {
        key: "Content-Type".into(),
        value: "application/json".into(),
    });
    let response = host::http_request(&host::HttpRequestData {
        provider_id: PROVIDER_ID.into(),
        account_id: Some(account_id.into()),
        method: "POST".into(),
        url: format!("{AG1_GATEWAY}?_={now_ms}&encoding=ag-1&sign={sign}"),
        headers,
        body,
    })
    .map_err(|error| format!("QQ ag-1 HTTP 调用失败: {error}"))?;
    if response.body.len() > MAX_HTTP_BODY_BYTES {
        return Err("QQ ag-1 响应超限".into());
    }
    if !(200..300).contains(&response.status) {
        return Err(format!("QQ ag-1 HTTP 状态异常: {}", response.status));
    }
    crypto::ag1::decode_response(&response.body)
}

/// Success requires the gateway, module and inner mutation code to be explicit
/// zeros. Missing code, 80105 or a nonzero subcode must never appear as success.
pub(crate) fn ag1_write_success(response: &Value, key: &str) -> bool {
    let Some(block) = response.get(key) else {
        return false;
    };
    response.get("code").and_then(Value::as_i64) == Some(0)
        && block.get("code").and_then(Value::as_i64) == Some(0)
        && block.pointer("/data/retCode").and_then(Value::as_i64) == Some(0)
        && block
            .get("subcode")
            .and_then(Value::as_i64)
            .is_none_or(|code| code == 0)
        && block
            .pointer("/data/subcode")
            .and_then(Value::as_i64)
            .is_none_or(|code| code == 0)
}

#[cfg(test)]
mod ag1_write_tests {
    use super::*;

    #[test]
    fn nested_write_codes_are_required() {
        let ok = serde_json::json!({"code":0,"like":{"code":0,"data":{"retCode":0}}});
        assert!(ag1_write_success(&ok, "like"));
        for invalid in [
            serde_json::json!({"code":0,"like":{"code":0,"data":{}}}),
            serde_json::json!({"code":0,"like":{"code":80105,"data":{"retCode":0}}}),
            serde_json::json!({"code":2000,"like":{"code":0,"data":{"retCode":0}}}),
            serde_json::json!({"like":{"code":0,"data":{"retCode":0}}}),
            serde_json::json!({"code":0,"like":{"code":0,"data":{"retCode":0,"subcode":99}}}),
        ] {
            assert!(!ag1_write_success(&invalid, "like"));
        }
    }
}

/// 搜索专用信封：请求 key = module 名且不带顶层 comm（实测规则）。
pub fn musicu_search_raw(
    session: &Session,
    query: &str,
    page_num: u32,
    num_per_page: u32,
) -> ProtocolResult<Value> {
    let mut param = Map::new();
    param.insert("search_type".into(), Value::from(0));
    param.insert("query".into(), Value::from(query));
    param.insert("page_num".into(), Value::from(page_num));
    param.insert("num_per_page".into(), Value::from(num_per_page));
    let mut block = Map::new();
    block.insert(
        "module".into(),
        Value::from("music.search.SearchCgiService"),
    );
    block.insert("method".into(), Value::from("DoSearchForQQMusicDesktop"));
    block.insert("param".into(), Value::Object(param));
    let mut envelope = Map::new();
    envelope.insert("music.search.SearchCgiService".into(), Value::Object(block));
    let body = serde_json::to_vec(&Value::Object(envelope))
        .map_err(|error| format!("搜索请求序列化失败: {error}"))?;
    send(session, None, MUSICU_GATEWAY, body)
}

/// 老 fcgi GET 通道（c.y.qq.com 歌词/主页等），自动剥离 JSONP 包裹。
pub fn fcgi_get(session: &Session, account_id: Option<&str>, url: &str) -> ProtocolResult<Value> {
    let mut headers = base_headers(session);
    if session.cookie.is_some() {
        // 老 fcgi 需要 URL 级 g_tk/loginUin，由调用方拼入 URL；这里只补 Content-Type 无关头。
        headers.push(types::KeyValue {
            key: "Accept".into(),
            value: "application/json".into(),
        });
    }
    let request = host::HttpRequestData {
        provider_id: PROVIDER_ID.into(),
        account_id: account_id.map(str::to_owned),
        method: "GET".into(),
        url: url.into(),
        headers,
        body: Vec::new(),
    };
    let response =
        host::http_request(&request).map_err(|error| format!("QQ音乐 fcgi 调用失败: {error}"))?;
    if response.body.len() > MAX_HTTP_BODY_BYTES {
        return Err(format!(
            "QQ音乐 fcgi 响应超过插件上限 {MAX_HTTP_BODY_BYTES} bytes"
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(format!("QQ音乐 fcgi HTTP 状态异常: {}", response.status));
    }
    let text = String::from_utf8_lossy(&response.body);
    let trimmed = text.trim();
    let json_text = if trimmed.starts_with("MusicJsonCallback(") {
        trimmed
            .strip_prefix("MusicJsonCallback(")
            .and_then(|rest| rest.strip_suffix(')'))
            .unwrap_or(trimmed)
    } else {
        trimmed
    };
    serde_json::from_str(json_text).map_err(|error| format!("QQ音乐 fcgi JSON 解析失败: {error}"))
}

/// 校验模块块 code；`2001`（搜索瞬时错误）由调用方重试，其余非 2xx 报错。
pub fn module_code(block: &Value) -> Option<i64> {
    block.get("code").and_then(Value::as_i64)
}

/// 逐层成功判定：外层 code → 模块块 code → `data.retCode`（写接口三层都可能是假成功）。
pub fn nested_write_success(module_block: &Value) -> bool {
    let outer_ok = module_code(module_block).is_none_or(|code| (200..300).contains(&code));
    let ret = module_block
        .pointer("/data/retCode")
        .or_else(|| module_block.pointer("/data/retcode"))
        .and_then(Value::as_i64);
    outer_ok && ret.is_none_or(|code| code == 0)
}
