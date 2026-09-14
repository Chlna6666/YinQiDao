use anyhow::{Result, anyhow, bail};

use super::{
    abi::{RecognitionRequest, RecognitionResult, RoutingPolicy, ServiceKind},
    client,
    frontend::{PluginCallFailure, PluginServiceFrontend, PluginSingleResult},
    host::runtime::{self, PluginCallKey},
};

const MAX_RECOGNITION_ALGORITHM_BYTES: usize = 128;
const MAX_RECOGNITION_FINGERPRINT_BYTES: usize = 512 * 1024;
const MAX_RECOGNITION_TRACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_RECOGNITION_TRACK_ARTISTS: usize = 128;
const MAX_RECOGNITION_SOURCE_ID_BYTES: usize = 4 * 1024;
const MAX_RECOGNITION_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;

impl PluginServiceFrontend {
    /// Run one Host-computed fingerprint through authenticated plugin Recognition routes.
    ///
    /// Route ordering, session eligibility and health gating come from the same frontend planner used
    /// by metadata/lyrics. The fingerprint is computed by the Host before this boundary; plugins do
    /// not receive filesystem paths or decoder access. Invalid guest results are isolated to their
    /// route and the next eligible account is tried before falling back to built-in recognition.
    pub async fn recognize(
        &self,
        request: &RecognitionRequest,
        policy: &RoutingPolicy,
    ) -> Result<PluginSingleResult<RecognitionResult>> {
        validate_recognition_request(request)?;
        let plan = self.plan(ServiceKind::Recognition, policy)?;

        let clients = client::global().unwrap_or_else(client::initialize);
        let Some(client) = clients.client()? else {
            return Ok(PluginSingleResult {
                value: None,
                route: None,
                plan,
                failures: Vec::new(),
                client_ready: false,
            });
        };
        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;

        let mut failures = Vec::new();
        for route in &plan.eligible_routes {
            let key = PluginCallKey::provider(&route.plugin_id, &route.provider_id);
            let call = client.recognize(
                &route.plugin_id,
                &route.provider_id,
                Some(&route.account_id),
                request,
            );
            match runtime.execute_guest_call(key, call).await {
                Ok(Some(result)) => match validate_recognition_result(route, &result) {
                    Ok(()) => {
                        return Ok(PluginSingleResult {
                            value: Some(result),
                            route: Some(route.clone()),
                            plan,
                            failures,
                            client_ready: true,
                        });
                    }
                    Err(error) => failures.push(PluginCallFailure {
                        route: route.clone(),
                        error: format!("Recognition 返回值非法: {error:#}"),
                    }),
                },
                Ok(None) => {}
                Err(error) => failures.push(PluginCallFailure {
                    route: route.clone(),
                    error: format!("{error:#}"),
                }),
            }
        }

        Ok(PluginSingleResult {
            value: None,
            route: None,
            plan,
            failures,
            client_ready: true,
        })
    }

    /// Cheap Host-only probe used before decoding audio solely for plugin recognition. It never
    /// invokes guest code and therefore remains safe on ordinary UI/worker planning paths.
    pub fn has_authenticated_recognition_route(&self, policy: &RoutingPolicy) -> Result<bool> {
        Ok(!self
            .plan(ServiceKind::Recognition, policy)?
            .eligible_routes
            .is_empty())
    }
}

fn validate_recognition_request(request: &RecognitionRequest) -> Result<()> {
    let algorithm = request.algorithm.trim();
    if algorithm.is_empty()
        || algorithm.len() > MAX_RECOGNITION_ALGORITHM_BYTES
        || algorithm.contains('\0')
    {
        bail!("Recognition algorithm 非法");
    }
    if request.fingerprint.is_empty() {
        bail!("Recognition fingerprint 为空");
    }
    if request.fingerprint.len() > MAX_RECOGNITION_FINGERPRINT_BYTES {
        bail!(
            "Recognition fingerprint 超过 {} bytes 限制",
            MAX_RECOGNITION_FINGERPRINT_BYTES
        );
    }
    if request.duration_ms == 0 || request.duration_ms > MAX_RECOGNITION_DURATION_MS {
        bail!("Recognition duration_ms 超出允许范围");
    }
    Ok(())
}

fn validate_recognition_result(
    route: &super::abi::PluginRoute,
    result: &RecognitionResult,
) -> Result<()> {
    if let Some(confidence) = result.confidence
        && (!confidence.is_finite() || !(0.0..=1.0).contains(&confidence))
    {
        bail!("Recognition confidence 必须位于 0..=1");
    }

    let track = &result.track;
    if track.source.provider_id != route.provider_id {
        bail!(
            "Recognition track provider 不匹配: expected={}, actual={}",
            route.provider_id,
            track.source.provider_id
        );
    }
    if track.source.source_id.trim().is_empty()
        || track.source.source_id.len() > MAX_RECOGNITION_SOURCE_ID_BYTES
        || track.source.source_id.contains('\0')
    {
        bail!("Recognition track source id 非法");
    }
    if track.title.trim().is_empty() || track.title.contains('\0') {
        bail!("Recognition track title 非法");
    }
    if track.artists.len() > MAX_RECOGNITION_TRACK_ARTISTS {
        bail!("Recognition track artists 数量超过限制");
    }
    if track
        .duration_ms
        .is_some_and(|duration_ms| duration_ms == 0 || duration_ms > MAX_RECOGNITION_DURATION_MS)
    {
        bail!("Recognition track duration_ms 超出允许范围");
    }

    let mut text_bytes = track
        .source
        .provider_id
        .len()
        .saturating_add(track.source.source_id.len())
        .saturating_add(track.title.len())
        .saturating_add(track.album.len())
        .saturating_add(track.isrc.as_ref().map_or(0, String::len))
        .saturating_add(track.cover_url.as_ref().map_or(0, String::len));
    for artist in &track.artists {
        if artist.contains('\0') {
            bail!("Recognition track artist 包含 NUL");
        }
        text_bytes = text_bytes.saturating_add(artist.len());
    }
    if text_bytes > MAX_RECOGNITION_TRACK_TEXT_BYTES {
        bail!(
            "Recognition track 文本超过 {} bytes 限制",
            MAX_RECOGNITION_TRACK_TEXT_BYTES
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::abi::{PluginRoute, RemoteTrack, SourceTrackRef};

    fn route() -> PluginRoute {
        PluginRoute {
            plugin_id: "plugin.test".into(),
            provider_id: "qqmusic".into(),
            account_id: "account".into(),
            priority: 0,
            is_default: true,
        }
    }

    fn result(confidence: Option<f32>) -> RecognitionResult {
        RecognitionResult {
            track: RemoteTrack {
                source: SourceTrackRef {
                    provider_id: "qqmusic".into(),
                    source_id: "mid".into(),
                },
                title: "Track".into(),
                artists: vec!["Artist".into()],
                duration_ms: Some(180_000),
                ..RemoteTrack::default()
            },
            confidence,
        }
    }

    #[test]
    fn recognition_request_is_bounded_before_guest_call() {
        assert!(
            validate_recognition_request(&RecognitionRequest {
                algorithm: "chromaprint-v1-compressed".into(),
                fingerprint: vec![1, 2, 3],
                duration_ms: 180_000,
            })
            .is_ok()
        );
        assert!(
            validate_recognition_request(&RecognitionRequest {
                algorithm: "chromaprint-v1-compressed".into(),
                fingerprint: vec![0; MAX_RECOGNITION_FINGERPRINT_BYTES + 1],
                duration_ms: 180_000,
            })
            .is_err()
        );
    }

    #[test]
    fn recognition_result_rejects_cross_provider_identity_and_invalid_confidence() {
        assert!(validate_recognition_result(&route(), &result(Some(0.9))).is_ok());
        assert!(validate_recognition_result(&route(), &result(Some(1.1))).is_err());

        let mut mismatched = result(Some(0.9));
        mismatched.track.source.provider_id = "netease".into();
        assert!(validate_recognition_result(&route(), &mismatched).is_err());
    }
}
