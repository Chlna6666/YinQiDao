use serde_json::Value;

use crate::{
    bindings::yinqidao::music_plugin::types,
    protocol,
};

/// 简单的 URL 编码实现，适用于标准 ASCII/UTF-8 字符
fn url_encode(input: &str) -> String {
    let mut output = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(byte as char);
            }
            b' ' => output.push('+'),
            _ => {
                output.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    output
}

/// 尝试通过第三方开源与网络搜索音源对变灰/无版权/VIP限制歌曲进行音源解灰
pub fn resolve_stream(
    song_id: u64,
    title: &str,
    artists: &[String],
    duration_ms: Option<u64>,
) -> Result<types::StreamDescriptor, String> {
    // 1. 尝试酷我音乐网络搜索与音源匹配（无额外鉴权，直接返回真实音频流）
    if let Ok(descriptor) = try_kuwo_match(title, artists, duration_ms) {
        return Ok(descriptor);
    }

    // 2. 尝试 GDStudio 开源音乐台解灰接口
    if let Ok(descriptor) = try_gdstudio_match(song_id) {
        return Ok(descriptor);
    }

    // 3. 尝试 MSLS 解灰接口
    if let Ok(descriptor) = try_msls_match(song_id) {
        return Ok(descriptor);
    }

    // 4. 尝试 Qijieya Meting 解灰接口
    if let Ok(descriptor) = try_qijieya_match(song_id) {
        return Ok(descriptor);
    }

    Err("网易云原曲受版权或 VIP 限制，且第三方音源解灰未能匹配到可用播放地址".into())
}

fn try_kuwo_match(
    title: &str,
    artists: &[String],
    duration_ms: Option<u64>,
) -> Result<types::StreamDescriptor, String> {
    if title.trim().is_empty() {
        return Err("缺少歌曲标题".into());
    }

    let artist_str = artists.first().map(|s| s.as_str()).unwrap_or("");
    let query = if artist_str.is_empty() {
        url_encode(title)
    } else {
        format!("{}+{}", url_encode(title), url_encode(artist_str))
    };

    let search_url = format!(
        "https://search.kuwo.cn/r.s?client=kt&all={query}&rformat=json&encoding=utf8&version=mbox&vipver=1&show_copyright_off=1&latest=0&ft=music&cluster=0&strategy=2012&pn=0&rn=5"
    );

    let search_body = protocol::http_get(
        &search_url,
        vec![types::KeyValue {
            key: "User-Agent".into(),
            value: "okhttp/3.10.0".into(),
        }],
    )?;

    let search_json: Value = serde_json::from_slice(&search_body)
        .map_err(|e| format!("解析酷我搜索结果失败: {e}"))?;

    let abslist = search_json
        .get("abslist")
        .and_then(Value::as_array)
        .ok_or_else(|| "酷我搜索未返回歌曲列表".to_string())?;

    let mut best_rid: Option<String> = None;
    let mut best_score = 0;

    for item in abslist {
        let name = item.get("SONGNAME").and_then(Value::as_str).unwrap_or("");
        let item_artist = item.get("ARTIST").and_then(Value::as_str).unwrap_or("");
        let rid = item
            .get("DC_TARGETID")
            .and_then(Value::as_str)
            .or_else(|| item.get("MUSICRID").and_then(Value::as_str))
            .unwrap_or("");

        if rid.is_empty() {
            continue;
        }

        let mut score = 0;
        if name.trim().eq_ignore_ascii_case(title.trim()) {
            score += 10;
        } else if name.contains(title) || title.contains(name) {
            score += 5;
        }

        if !artist_str.is_empty()
            && (item_artist.contains(artist_str) || artist_str.contains(item_artist))
        {
            score += 5;
        }

        if let Some(target_dur) = duration_ms {
            let dur_sec = item
                .get("DURATION")
                .and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .or_else(|| v.as_u64())
                })
                .unwrap_or(0);
            if dur_sec > 0 {
                let diff = (dur_sec * 1000).abs_diff(target_dur);
                if diff <= 3000 {
                    score += 5;
                } else if diff <= 8000 {
                    score += 2;
                }
            }
        }

        if score > best_score {
            best_score = score;
            best_rid = Some(rid.to_owned());
        }
    }

    let rid = best_rid.ok_or_else(|| "酷我搜索未找到足够匹配的曲目".to_string())?;
    let clean_rid = rid.trim_start_matches("MUSIC_");

    let play_url_endpoint = format!(
        "https://antiserver.kuwo.cn/anti.s?type=convert_url&rid={clean_rid}&format=mp3&response=url"
    );

    let play_body = protocol::http_get(
        &play_url_endpoint,
        vec![types::KeyValue {
            key: "User-Agent".into(),
            value: "okhttp/3.10.0".into(),
        }],
    )?;

    let play_url =
        String::from_utf8(play_body).map_err(|e| format!("解析播放地址失败: {e}"))?;
    let play_url = play_url.trim();

    if !play_url.starts_with("http") {
        return Err("酷我未返回有效音频链接".into());
    }

    Ok(types::StreamDescriptor {
        url: play_url.to_owned(),
        headers: vec![types::KeyValue {
            key: "User-Agent".into(),
            value: "okhttp/3.10.0".into(),
        }],
        codec: Some("mp3".into()),
        bitrate: Some(320_000),
        sample_rate: Some(44_100),
        channels: Some(2),
        expires_at_ms: None,
    })
}

fn try_gdstudio_match(song_id: u64) -> Result<types::StreamDescriptor, String> {
    let url = format!(
        "https://music-api.gdstudio.xyz/api.php?types=url&source=netease&id={song_id}&br=320"
    );
    let body = protocol::http_get(
        &url,
        vec![types::KeyValue {
            key: "User-Agent".into(),
            value: "Mozilla/5.0".into(),
        }],
    )?;
    let json: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let stream_url = json
        .get("url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("http"))
        .ok_or_else(|| "GDStudio 未返回有效播放地址".to_string())?;

    Ok(types::StreamDescriptor {
        url: stream_url.to_owned(),
        headers: vec![types::KeyValue {
            key: "User-Agent".into(),
            value: "Mozilla/5.0".into(),
        }],
        codec: Some("mp3".into()),
        bitrate: Some(320_000),
        sample_rate: Some(44_100),
        channels: Some(2),
        expires_at_ms: None,
    })
}

fn try_msls_match(song_id: u64) -> Result<types::StreamDescriptor, String> {
    let url = format!("https://api.msls1441.com/?type=url&id={song_id}");
    let body = protocol::http_get(
        &url,
        vec![types::KeyValue {
            key: "User-Agent".into(),
            value: "Mozilla/5.0".into(),
        }],
    )?;
    let json: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let stream_url = json
        .get("url")
        .and_then(Value::as_str)
        .or_else(|| json.pointer("/data/url").and_then(Value::as_str))
        .filter(|u| u.starts_with("http"))
        .ok_or_else(|| "MSLS 未返回有效播放地址".to_string())?;

    Ok(types::StreamDescriptor {
        url: stream_url.to_owned(),
        headers: vec![types::KeyValue {
            key: "User-Agent".into(),
            value: "Mozilla/5.0".into(),
        }],
        codec: Some("mp3".into()),
        bitrate: Some(320_000),
        sample_rate: Some(44_100),
        channels: Some(2),
        expires_at_ms: None,
    })
}

fn try_qijieya_match(song_id: u64) -> Result<types::StreamDescriptor, String> {
    let url = format!("https://api.qijieya.cn/meting/?type=url&id={song_id}");
    let body = protocol::http_get(
        &url,
        vec![types::KeyValue {
            key: "User-Agent".into(),
            value: "Mozilla/5.0".into(),
        }],
    )?;
    let json: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let stream_url = json
        .get("url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("http"))
        .ok_or_else(|| "Qijieya 未返回有效播放地址".to_string())?;

    Ok(types::StreamDescriptor {
        url: stream_url.to_owned(),
        headers: vec![types::KeyValue {
            key: "User-Agent".into(),
            value: "Mozilla/5.0".into(),
        }],
        codec: Some("mp3".into()),
        bitrate: Some(320_000),
        sample_rate: Some(44_100),
        channels: Some(2),
        expires_at_ms: None,
    })
}
