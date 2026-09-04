use std::{collections::HashMap, fs, path::Path, sync::Arc};

use lofty::{prelude::TaggedFileExt, tag::ItemKey};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LyricsDocument {
    pub plain: Option<String>,
    pub synced: Option<String>,
    pub source: String,
    timed: Arc<[LyricLine]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LyricLine {
    pub timestamp_ms: u64,
    pub text: String,
}

impl LyricsDocument {
    pub fn from_sources(
        plain: Option<String>,
        synced: Option<String>,
        translation: Option<String>,
        source: impl Into<String>,
    ) -> Self {
        let synced = match (synced, translation) {
            (Some(original), Some(translation)) => {
                Some(merge_translated_lrc(&original, &translation))
            }
            (Some(original), None) => Some(original),
            (None, Some(translation)) => Some(translation),
            (None, None) => None,
        };
        let timed = synced.as_deref().map(parse_lrc).unwrap_or_default().into();
        Self {
            plain,
            synced,
            source: source.into(),
            timed,
        }
    }

    #[allow(dead_code)]
    pub fn best_text(&self) -> Option<&str> {
        self.synced.as_deref().or(self.plain.as_deref())
    }

    pub fn timed_lines(&self) -> &[LyricLine] {
        &self.timed
    }
}

/// 自动识别并解码字节流（支持 UTF-8 BOM, UTF-8, GBK/GB2312/GB18030）
pub fn decode_bytes_to_string(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // 1. 优先检查 BOM 头 (UTF-8 / UTF-16)
    if let Some((encoding, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        let (cow, _, _) = encoding.decode(&bytes[bom_len..]);
        return cow.into_owned();
    }
    // 2. 尝试原生 UTF-8 解码
    if let Ok(utf8_str) = std::str::from_utf8(bytes) {
        return utf8_str.to_owned();
    }
    // 3. 回退至 GB18030（兼容并完整覆盖 GBK 与 GB2312）
    let (cow, _, _) = encoding_rs::GB18030.decode(bytes);
    cow.into_owned()
}

pub fn read_local(path: &Path) -> Option<LyricsDocument> {
    let sidecar = path.with_extension("lrc");
    if let Ok(bytes) = fs::read(&sidecar) {
        let text = decode_bytes_to_string(&bytes);
        if !text.trim().is_empty() {
            let synced = (!parse_lrc(&text).is_empty()).then(|| text.clone());
            return Some(LyricsDocument::from_sources(
                synced.is_none().then_some(text),
                synced,
                None,
                "本地 LRC",
            ));
        }
    }

    let tagged = lofty::read_from_path(path).ok()?;
    let lyrics = tagged
        .tags()
        .iter()
        .find_map(|tag| tag.get_string(&ItemKey::Lyrics))?
        .trim()
        .to_owned();
    if lyrics.is_empty() {
        return None;
    }
    let synced = (!parse_lrc(&lyrics).is_empty()).then(|| lyrics.clone());
    Some(LyricsDocument::from_sources(
        synced.is_none().then_some(lyrics.clone()),
        synced,
        None,
        "内嵌歌词",
    ))
}

pub fn merge_translated_lrc(original: &str, translation: &str) -> String {
    let translated = parse_lrc(translation).into_iter().fold(
        HashMap::<u64, Vec<String>>::new(),
        |mut lines, line| {
            lines.entry(line.timestamp_ms).or_default().push(line.text);
            lines
        },
    );
    let mut merged = String::with_capacity(original.len() + translation.len());

    for line in original.lines() {
        merged.push_str(line);
        merged.push('\n');
        let timestamps = line_timestamps(line);
        for timestamp in timestamps {
            if let Some(lines) = translated.get(&timestamp) {
                for text in lines {
                    merged.push_str(&format_timestamp(timestamp));
                    merged.push_str(text);
                    merged.push('\n');
                }
            }
        }
    }

    if merged.is_empty() {
        translation.trim_end().to_owned()
    } else {
        merged.truncate(merged.trim_end().len());
        merged
    }
}

/// 全功能 LRC 解析引擎：
/// 1. 支持 [offset:+/-ms] 偏移补偿
/// 2. 兼容多种时间格式（mm:ss.xx, mm:ss:xx, mm:ss.xxx, mm:ss, hh:mm:ss.xx）
/// 3. 支持一行多时间标签（含空格分隔）
/// 4. 过滤卡拉OK行内字级别时间戳（如 `<00:12.34>`）
/// 5. 过滤常见元数据头
pub fn parse_lrc(input: &str) -> Vec<LyricLine> {
    let global_offset = parse_offset(input);
    let mut lines = Vec::new();

    for line in input.lines() {
        let mut remaining = line.trim();
        let mut timestamps = Vec::new();

        loop {
            remaining = remaining.trim_start();
            if let Some(rest) = remaining.strip_prefix('[')
                && let Some(end) = rest.find(']')
            {
                let stamp = &rest[..end];
                if let Some(timestamp_ms) = parse_timestamp(stamp) {
                    timestamps.push(timestamp_ms);
                    remaining = &rest[end + 1..];
                    continue;
                }
            }
            break;
        }

        if timestamps.is_empty() {
            continue;
        }

        // 清理行内卡拉OK标签并提取纯文本
        let text = clean_inline_tags(remaining);

        for raw_ts in timestamps {
            let final_ts = if global_offset >= 0 {
                raw_ts.saturating_add(global_offset as u64)
            } else {
                raw_ts.saturating_sub((-global_offset) as u64)
            };
            lines.push(LyricLine {
                timestamp_ms: final_ts,
                text: text.clone(),
            });
        }
    }

    lines.sort_by_key(|line| line.timestamp_ms);
    lines
}

fn parse_offset(input: &str) -> i64 {
    for line in input.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("[offset:")
            .or_else(|| trimmed.strip_prefix("[OFFSET:"))
            && let Some(end) = rest.find(']')
        {
            let offset_str = rest[..end].trim();
            if let Ok(offset) = offset_str.parse::<i64>() {
                return offset;
            }
        }
    }
    0
}

fn line_timestamps(line: &str) -> Vec<u64> {
    let mut remaining = line.trim();
    let mut timestamps = Vec::new();
    while let Some(rest) = remaining.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            break;
        };
        let Some(timestamp) = parse_timestamp(&rest[..end]) else {
            break;
        };
        timestamps.push(timestamp);
        remaining = &rest[end + 1..];
    }
    timestamps
}

fn format_timestamp(timestamp_ms: u64) -> String {
    let minutes = timestamp_ms / 60_000;
    let seconds = (timestamp_ms % 60_000) / 1_000;
    let hundredths = (timestamp_ms % 1_000) / 10;
    format!("[{minutes:02}:{seconds:02}.{hundredths:02}]")
}

fn parse_timestamp(value: &str) -> Option<u64> {
    let value = value.trim();
    let parts: Vec<&str> = value.split(':').collect();
    match parts.len() {
        2 => {
            let minutes = parts[0].parse::<u64>().ok()?;
            let (seconds, ms) = parse_seconds_and_fraction(parts[1])?;
            (seconds < 60).then(|| minutes * 60_000 + seconds * 1_000 + ms)
        }
        3 => {
            if let (Ok(p0), Ok(p1)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>()) {
                if parts[2].contains('.') {
                    // hh:mm:ss.xx
                    let (sec, ms) = parse_seconds_and_fraction(parts[2])?;
                    (p1 < 60 && sec < 60).then(|| p0 * 3_600_000 + p1 * 60_000 + sec * 1_000 + ms)
                } else if let Ok(frac) = parts[2].parse::<u64>() {
                    // mm:ss:xx (分:秒:毫秒/百分秒)
                    let ms = if parts[2].len() == 2 {
                        frac * 10
                    } else if parts[2].len() == 1 {
                        frac * 100
                    } else {
                        frac
                    };
                    (p1 < 60).then(|| p0 * 60_000 + p1 * 1_000 + ms)
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_seconds_and_fraction(val: &str) -> Option<(u64, u64)> {
    if let Some((sec_str, frac_str)) = val.split_once('.') {
        let sec = sec_str.parse::<u64>().ok()?;
        let frac_val = frac_str.parse::<u64>().ok()?;
        let ms = match frac_str.len() {
            1 => frac_val * 100,
            2 => frac_val * 10,
            3 => frac_val,
            len if len > 3 => frac_str[..3].parse::<u64>().ok()?,
            _ => 0,
        };
        Some((sec, ms))
    } else {
        let sec = val.parse::<u64>().ok()?;
        Some((sec, 0))
    }
}

fn clean_inline_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' && in_tag {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_parser_orders_multiple_timestamps() {
        let lines = parse_lrc("[00:03.20][00:01.00]同一句\n[00:02.50]第二句\n[ar:歌手]");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].timestamp_ms, 1_000);
        assert_eq!(lines[1].text, "第二句");
        assert_eq!(lines[2].timestamp_ms, 3_200);
    }

    #[test]
    fn lrc_parser_handles_offset_and_colon_ms() {
        let input = "[offset:500]\n[01:10:50]冒号百分秒测试\n[01:20.250]三位毫秒";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 2);
        // 01:10:50 = 70500 + 500 = 71000ms
        assert_eq!(lines[0].timestamp_ms, 71_000);
        assert_eq!(lines[0].text, "冒号百分秒测试");
        // 01:20.250 = 80250 + 500 = 80750ms
        assert_eq!(lines[1].timestamp_ms, 80_750);
    }

    #[test]
    fn lrc_parser_cleans_karaoke_tags() {
        let input = "[00:10.00]<00:10.00>你好 <00:10.50>世界";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "你好 世界");
    }

    #[test]
    fn decode_bytes_detects_gbk() {
        // "你好" in GBK is [0xC4, 0xE3, 0xBA, 0xC3]
        let gbk_bytes = [0xC4, 0xE3, 0xBA, 0xC3];
        let decoded = decode_bytes_to_string(&gbk_bytes);
        assert_eq!(decoded, "你好");
    }

    #[test]
    fn translated_lrc_is_kept_on_the_same_timeline() {
        let document = LyricsDocument::from_sources(
            None,
            Some("[00:01.00]你好\n[00:02.00]再见".into()),
            Some("[00:01.00]Hello\n[00:02.00]Goodbye".into()),
            "测试",
        );
        assert_eq!(document.timed_lines()[0].text, "你好");
        assert_eq!(document.timed_lines()[1].text, "Hello");
        assert_eq!(document.timed_lines()[2].text, "再见");
        assert_eq!(document.timed_lines()[3].text, "Goodbye");
    }
}
