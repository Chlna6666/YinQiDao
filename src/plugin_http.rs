use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use reqwest::{
    Client, Method, Response, Url,
    header::{
        AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HeaderMap, HeaderName,
        HeaderValue, LOCATION, PROXY_AUTHORIZATION, TE, TRAILER, TRANSFER_ENCODING, UPGRADE,
    },
    redirect::Policy,
};

use crate::{
    plugin_security::{
        PluginPermissionGrant, authorize_http_target, authorize_redirect,
    },
    plugins::{KeyValue, PluginManifest},
};

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_MAX_REQUEST_BODY: usize = 2 * 1024 * 1024;
const DEFAULT_MAX_RESPONSE_BODY: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_HEADER_COUNT: usize = 96;
const DEFAULT_MAX_HEADER_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_REDIRECTS: usize = 5;

#[derive(Clone, Debug)]
pub struct PluginHttpLimits {
    pub connect_timeout: Duration,
    /// Total deadline for the complete call, including DNS, redirects and response streaming.
    pub request_timeout: Duration,
    pub max_request_body: usize,
    pub max_response_body: usize,
    pub max_header_count: usize,
    pub max_header_bytes: usize,
    pub max_redirects: usize,
}

impl Default for PluginHttpLimits {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_request_body: DEFAULT_MAX_REQUEST_BODY,
            max_response_body: DEFAULT_MAX_RESPONSE_BODY,
            max_header_count: DEFAULT_MAX_HEADER_COUNT,
            max_header_bytes: DEFAULT_MAX_HEADER_BYTES,
            max_redirects: DEFAULT_MAX_REDIRECTS,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PluginHttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<KeyValue>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct PluginHttpResponse {
    pub status: u16,
    pub headers: Vec<KeyValue>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct PluginHttpExecutor {
    limits: PluginHttpLimits,
}

impl PluginHttpExecutor {
    pub fn new(limits: PluginHttpLimits) -> Self {
        Self { limits }
    }

    /// Execute one Host-mediated plugin request under a total deadline.
    ///
    /// DNS is resolved by the Host, private/special addresses are removed, and the surviving
    /// addresses are pinned into the reqwest client for this hop. Reqwest automatic redirects are
    /// disabled so every redirect repeats permission + DNS checks before another socket is opened.
    pub async fn execute(
        &self,
        manifest: &PluginManifest,
        grant: &PluginPermissionGrant,
        request: PluginHttpRequest,
    ) -> Result<PluginHttpResponse> {
        tokio::time::timeout(
            self.limits.request_timeout,
            self.execute_inner(manifest, grant, request),
        )
        .await
        .with_context(|| {
            format!(
                "插件 HTTP 调用超过总时限 {} ms",
                self.limits.request_timeout.as_millis()
            )
        })?
    }

    async fn execute_inner(
        &self,
        manifest: &PluginManifest,
        grant: &PluginPermissionGrant,
        request: PluginHttpRequest,
    ) -> Result<PluginHttpResponse> {
        if request.body.len() > self.limits.max_request_body {
            bail!("插件 HTTP request body 超过 {} bytes", self.limits.max_request_body);
        }

        let mut method = parse_method(&request.method)?;
        let mut url = Url::parse(&request.url).context("插件 HTTP URL 非法")?;
        let mut headers = build_request_headers(&request.headers, &self.limits)?;
        let mut body = request.body;

        for redirect_count in 0..=self.limits.max_redirects {
            let authorized = if redirect_count == 0 {
                authorize_http_target(manifest, grant, &url)?
            } else {
                authorize_redirect(manifest, grant, &url)?
            };
            let addresses = resolve_public_addresses(&authorized.host).await?;
            let client = build_pinned_client(&authorized.host, &addresses, &self.limits)?;

            let response = client
                .request(method.clone(), authorized.url.clone())
                .headers(headers.clone())
                .body(body.clone())
                .send()
                .await
                .with_context(|| format!("插件 HTTP 请求失败: {}", authorized.url))?;

            let status = response.status().as_u16();
            if !is_followable_redirect(status) {
                return collect_response(response, &self.limits).await;
            }
            if redirect_count == self.limits.max_redirects {
                bail!("插件 HTTP redirect 超过 {} 次", self.limits.max_redirects);
            }

            let location = response
                .headers()
                .get(LOCATION)
                .ok_or_else(|| anyhow::anyhow!("插件 HTTP redirect 缺少 Location"))?
                .to_str()
                .context("插件 HTTP redirect Location 不是有效文本")?;
            let next_url = authorized
                .url
                .join(location)
                .context("插件 HTTP redirect Location 非法")?;
            // Authorize before mutating headers/method so a rejected redirect cannot affect the next
            // caller-visible state or cause another DNS lookup.
            authorize_redirect(manifest, grant, &next_url)?;

            let host_changed = authorized.url.host_str() != next_url.host_str();
            if host_changed {
                strip_cross_origin_credentials(&mut headers);
            }
            rewrite_redirect_request(status, &mut method, &mut body, &mut headers);
            url = next_url;
        }

        unreachable!("redirect loop always returns or errors")
    }
}

fn is_followable_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn parse_method(value: &str) -> Result<Method> {
    let method = Method::from_bytes(value.trim().as_bytes()).context("插件 HTTP method 非法")?;
    if method == Method::CONNECT || method == Method::TRACE {
        bail!("插件 HTTP 禁止 CONNECT/TRACE");
    }
    Ok(method)
}

fn build_request_headers(values: &[KeyValue], limits: &PluginHttpLimits) -> Result<HeaderMap> {
    if values.len() > limits.max_header_count {
        bail!("插件 HTTP request header 数量超过限制");
    }
    let mut total_bytes = 0usize;
    let mut headers = HeaderMap::with_capacity(values.len());
    for pair in values {
        total_bytes = total_bytes
            .saturating_add(pair.key.len())
            .saturating_add(pair.value.len());
        if total_bytes > limits.max_header_bytes {
            bail!("插件 HTTP request header 总大小超过限制");
        }
        let name = HeaderName::from_bytes(pair.key.trim().as_bytes())
            .with_context(|| format!("插件 HTTP header 名非法: {}", pair.key))?;
        if forbidden_request_header(&name) {
            bail!("插件 HTTP 禁止自行设置 header: {name}");
        }
        let value = HeaderValue::from_bytes(pair.value.as_bytes())
            .with_context(|| format!("插件 HTTP header 值非法: {name}"))?;
        headers.append(name, value);
    }
    Ok(headers)
}

fn forbidden_request_header(name: &HeaderName) -> bool {
    name == CONNECTION
        || name == CONTENT_LENGTH
        || name == TRANSFER_ENCODING
        || name == PROXY_AUTHORIZATION
        || name == UPGRADE
        || name == TE
        || name == TRAILER
        || name.as_str().eq_ignore_ascii_case("host")
        || name.as_str().eq_ignore_ascii_case("proxy-connection")
}

fn strip_cross_origin_credentials(headers: &mut HeaderMap) {
    headers.remove(AUTHORIZATION);
    headers.remove(COOKIE);
    headers.remove(PROXY_AUTHORIZATION);
}

fn rewrite_redirect_request(
    status: u16,
    method: &mut Method,
    body: &mut Vec<u8>,
    headers: &mut HeaderMap,
) {
    let switch_to_get = status == 303 || ((status == 301 || status == 302) && *method == Method::POST);
    if switch_to_get && *method != Method::HEAD {
        *method = Method::GET;
        body.clear();
        headers.remove(CONTENT_TYPE);
        headers.remove(CONTENT_LENGTH);
        headers.remove(TRANSFER_ENCODING);
    }
}

async fn resolve_public_addresses(host: &str) -> Result<Vec<SocketAddr>> {
    let host = host.to_owned();
    let display_host = host.clone();
    let addresses = tokio::task::spawn_blocking(move || {
        (host.as_str(), 443)
            .to_socket_addrs()
            .map(|addresses| addresses.collect::<Vec<_>>())
    })
    .await
    .context("等待插件 HTTP DNS 解析任务失败")?
    .with_context(|| format!("插件 HTTP DNS 解析失败: {display_host}"))?;

    // Pin only the vetted subset. A DNS answer containing both public and private addresses cannot
    // make reqwest fall back to a private address because those addresses are never passed to it.
    let mut public = addresses
        .into_iter()
        .filter(|address| ip_is_public(address.ip()))
        .collect::<Vec<_>>();
    public.sort_unstable();
    public.dedup();
    if public.is_empty() {
        bail!("插件 HTTP DNS 解析结果没有可访问的公网地址: {display_host}");
    }
    Ok(public)
}

fn build_pinned_client(
    host: &str,
    addresses: &[SocketAddr],
    limits: &PluginHttpLimits,
) -> Result<Client> {
    Client::builder()
        // Plugin networking must not inherit HTTP(S)_PROXY/ALL_PROXY from the launcher process.
        // Explicit Host proxy support can be added later with the same destination checks.
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(limits.connect_timeout)
        .timeout(limits.request_timeout)
        .resolve_to_addrs(host, addresses)
        .build()
        .context("创建插件 HTTP 客户端失败")
}

async fn collect_response(
    mut response: Response,
    limits: &PluginHttpLimits,
) -> Result<PluginHttpResponse> {
    if response
        .content_length()
        .is_some_and(|length| length > limits.max_response_body as u64)
    {
        bail!("插件 HTTP response Content-Length 超过限制");
    }

    let headers = collect_response_headers(response.headers(), limits)?;
    let status = response.status().as_u16();
    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(limits.max_response_body);
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response.chunk().await.context("读取插件 HTTP response 失败")? {
        if body.len().saturating_add(chunk.len()) > limits.max_response_body {
            bail!("插件 HTTP response body 超过 {} bytes", limits.max_response_body);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(PluginHttpResponse {
        status,
        headers,
        body,
    })
}

fn collect_response_headers(headers: &HeaderMap, limits: &PluginHttpLimits) -> Result<Vec<KeyValue>> {
    if headers.len() > limits.max_header_count {
        bail!("插件 HTTP response header 数量超过限制");
    }
    let mut output = Vec::with_capacity(headers.len());
    let mut total_bytes = 0usize;
    for (name, value) in headers {
        let Ok(value) = value.to_str() else {
            continue;
        };
        total_bytes = total_bytes
            .saturating_add(name.as_str().len())
            .saturating_add(value.len());
        if total_bytes > limits.max_header_bytes {
            bail!("插件 HTTP response header 总大小超过限制");
        }
        output.push(KeyValue {
            key: name.as_str().to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(output)
}

fn ip_is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ipv4_is_public(ip),
        IpAddr::V6(ip) => ipv6_is_public(ip),
    }
}

fn ipv4_is_public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || a >= 240
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19)))
}

fn ipv6_is_public(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return ipv4_is_public(mapped);
    }
    let segments = ip.segments();
    let well_known_nat64 = segments[0] == 0x0064
        && segments[1] == 0xff9b
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0;
    let local_nat64 = segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2] == 0x0001;
    let discard_only = segments[0] == 0x0100
        && segments[1] == 0
        && segments[2] == 0
        && segments[3] == 0;
    let teredo = segments[0] == 0x2001 && segments[1] == 0;
    let benchmarking = segments[0] == 0x2001 && segments[1] == 0x0002;
    let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
    let orchid = segments[0] == 0x2001
        && ((0x0010..=0x001f).contains(&segments[1])
            || (0x0020..=0x002f).contains(&segments[1]));
    let six_to_four = segments[0] == 0x2002;

    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || well_known_nat64
        || local_nat64
        || discard_only
        || teredo
        || benchmarking
        || documentation
        || orchid
        || six_to_four)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_ipv4_ranges_are_not_public() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.1.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "192.0.0.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
        ] {
            assert!(!ip_is_public(address.parse().expect("ip")), "{address}");
        }
        assert!(ip_is_public("1.1.1.1".parse().expect("ip")));
    }

    #[test]
    fn special_ipv6_ranges_are_not_public() {
        for address in [
            "::1",
            "fe80::1",
            "fc00::1",
            "64:ff9b::a00:1",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:2::1",
            "2001:db8::1",
            "2001:10::1",
            "2001:20::1",
            "2002::1",
        ] {
            assert!(!ip_is_public(address.parse().expect("ip")), "{address}");
        }
        assert!(ip_is_public("2606:4700:4700::1111".parse().expect("ip")));
    }

    #[test]
    fn not_modified_is_not_treated_as_redirect() {
        assert!(!is_followable_redirect(304));
        assert!(is_followable_redirect(301));
        assert!(is_followable_redirect(308));
    }

    #[test]
    fn redirect_to_get_drops_entity_body() {
        let mut method = Method::POST;
        let mut body = b"payload".to_vec();
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        rewrite_redirect_request(303, &mut method, &mut body, &mut headers);
        assert_eq!(method, Method::GET);
        assert!(body.is_empty());
        assert!(!headers.contains_key(CONTENT_TYPE));
    }

    #[test]
    fn cross_origin_redirect_drops_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        headers.insert(COOKIE, HeaderValue::from_static("sid=secret"));
        strip_cross_origin_credentials(&mut headers);
        assert!(!headers.contains_key(AUTHORIZATION));
        assert!(!headers.contains_key(COOKIE));
    }
}
