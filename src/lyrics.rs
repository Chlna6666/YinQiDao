use std::{fs, path::Path, sync::Arc};

use lofty::{prelude::TaggedFileExt, tag::ItemKey};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LyricsDocument {
    pub plain: Option<String>,
    pub synced: Option<String>,
    pub translation: Option<String>,
    pub source: String,
    timed: Arc<[LyricLine]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LyricWord {
    /// Absolute transport timestamp for the beginning of this enhanced-LRC segment.
    pub timestamp_ms: u64,
    /// Segment text exactly as it appeared between two inline timestamps. Leading/trailing
    /// whitespace is intentionally preserved so the UI can rebuild the authored line without
    /// inserting synthetic gaps between CJK/Latin segments.
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LyricLine {
    pub timestamp_ms: u64,
    pub text: String,
    pub translation: Option<String>,
    /// Enhanced-LRC inline timing (`<mm:ss.xx>word`) when the source provides it. A plain LRC line
    /// leaves this empty; consumers must not invent word timing from character count.
    pub words: Arc<[LyricWord]>,
}

impl LyricsDocument {
    pub fn from_sources(
        plain: Option<String>,
        synced: Option<String>,
        translation: Option<String>,
        source: impl Into<String>,
    ) -> Self {
        let source = source.into();
        let timed = match (synced.as_deref(), translation.as_deref()) {
            (Some(original), Some(translated)) => pair_translated_lrc(original, translated),
            (Some(original), None) if is_legacy_bilingual_source(&source) => {
                collapse_legacy_bilingual_lrc(original)
            }
            (Some(original), None) => parse_lrc(original),
            (None, Some(translated)) => parse_lrc(translated),
            (None, None) => Vec::new(),
        }
        .into();
        Self {
            plain,
            synced,
            translation,
            source,
            timed,
        }
    }

    #[allow(dead_code)]
    pub fn best_text(&self) -> Option<&str> {
        self.synced
            .as_deref()
            .or(self.translation.as_deref())
            .or(self.plain.as_deref())
    }

    pub fn timed_lines(&self) -> &[LyricLine] {
        &self.timed
    }

    pub fn has_translation(&self) -> bool {
        self.timed.iter().any(|line| {
            line.translation
                .as_deref()
                .is_some_and(|translation| !translation.trim().is_empty())
        })
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

const TRANSLATION_SYNC_TOLERANCE_MS: u64 = 1_500;

fn pair_translated_lrc(original: &str, translated: &str) -> Vec<LyricLine> {
    let mut primary = parse_lrc(original);
    let translations = parse_lrc(translated);

    // Provider translation tracks are authored as a parallel sequence. When both parsed streams
    // contain the same number of timed rows, sequence identity is stronger than timestamp equality:
    // some services apply a different global offset or round timestamps independently. Pairing by
    // index keeps the Chinese subtitle attached to the authored primary line instead of silently
    // dropping it because the clocks drift by more than the proximity tolerance.
    if primary.len() == translations.len() {
        for (line, translation) in primary.iter_mut().zip(&translations) {
            attach_translation(line, translation);
        }
        return primary;
    }

    // If one provider omits/adds a few timed rows, fall back to one-to-one nearest-timestamp
    // matching. Never inject an unmatched translation as a new primary lyric row: doing so changes
    // the playback timeline and makes the immersive view alternate between original/translation.
    let mut used = vec![false; translations.len()];
    for line in &mut primary {
        let best = translations
            .iter()
            .enumerate()
            .filter(|(index, candidate)| {
                !used[*index]
                    && candidate.timestamp_ms.abs_diff(line.timestamp_ms)
                        <= TRANSLATION_SYNC_TOLERANCE_MS
            })
            .min_by_key(|(_, candidate)| candidate.timestamp_ms.abs_diff(line.timestamp_ms))
            .map(|(index, _)| index);
        if let Some(index) = best {
            used[index] = true;
            attach_translation(line, &translations[index]);
        }
    }
    primary
}

fn attach_translation(line: &mut LyricLine, translation: &LyricLine) {
    let value = translation.text.trim();
    if !value.is_empty() && value != line.text.trim() {
        line.translation = Some(value.to_owned());
    }
}

fn is_legacy_bilingual_source(source: &str) -> bool {
    matches!(source, "网易云音乐" | "QQ音乐" | "缓存")
}

fn collapse_legacy_bilingual_lrc(input: &str) -> Vec<LyricLine> {
    let mut collapsed: Vec<LyricLine> = Vec::new();
    for line in parse_lrc(input) {
        if let Some(previous) = collapsed.last_mut()
            && previous.timestamp_ms == line.timestamp_ms
            && previous.translation.is_none()
            && previous.text.trim() != line.text.trim()
        {
            previous.translation = Some(line.text);
            continue;
        }
        collapsed.push(line);
    }
    collapsed
}

/// 全功能 LRC 解析引擎：
/// 1. 支持 [offset:+/-ms] 偏移补偿
/// 2. 兼容多种时间格式（mm:ss.xx, mm:ss:xx, mm:ss.xxx, mm:ss, hh:mm:ss.xx）
/// 3. 支持一行多时间标签（含空格分隔）
/// 4. 保留 Enhanced LRC 行内逐词时间戳（如 `<00:12.34>你<00:12.60>好`）
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

        let (enhanced_text, enhanced_words) = parse_inline_words(remaining, global_offset);
        let text = if enhanced_words.is_empty() {
            clean_inline_tags(remaining)
        } else {
            clean_inline_tags(&enhanced_text)
        };
        // Inline timestamps are absolute transport times. If an LRC line contains multiple outer
        // timestamps, the same inline timeline cannot truthfully describe all repeated instances;
        // retain the line text but deliberately drop ambiguous word timing for those copies.
        let words: Arc<[LyricWord]> = if timestamps.len() == 1 {
            enhanced_words.into()
        } else {
            Arc::from([])
        };

        for raw_ts in timestamps {
            lines.push(LyricLine {
                timestamp_ms: apply_offset(raw_ts, global_offset),
                text: text.clone(),
                translation: None,
                words: words.clone(),
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

#[inline]
fn apply_offset(timestamp_ms: u64, offset_ms: i64) -> u64 {
    if offset_ms >= 0 {
        timestamp_ms.saturating_add(offset_ms as u64)
    } else {
        timestamp_ms.saturating_sub((-offset_ms) as u64)
    }
}

fn parse_inline_words(input: &str, offset_ms: i64) -> (String, Vec<LyricWord>) {
    let mut plain = String::with_capacity(input.len());
    let mut words = Vec::new();
    let mut cursor = 0usize;

    while cursor < input.len() {
        let Some(relative_start) = input[cursor..].find('<') else {
            plain.push_str(&input[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        plain.push_str(&input[cursor..start]);
        let Some(relative_end) = input[start + 1..].find('>') else {
            plain.push_str(&input[start..]);
            break;
        };
        let end = start + 1 + relative_end;
        let stamp = &input[start + 1..end];
        let Some(timestamp_ms) = parse_timestamp(stamp) else {
            plain.push_str(&input[start..=end]);
            cursor = end + 1;
            continue;
        };

        let segment_start = end + 1;
        let segment_end = input[segment_start..]
            .find('<')
            .map_or(input.len(), |relative| segment_start + relative);
        let segment = &input[segment_start..segment_end];
        plain.push_str(segment);
        if !segment.is_empty() {
            words.push(LyricWord {
                timestamp_ms: apply_offset(timestamp_ms, offset_ms),
                text: segment.to_owned(),
            });
        }
        cursor = segment_end;
    }

    (plain.trim().to_owned(), words)
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
    fn lrc_parser_preserves_enhanced_word_timing() {
        let input = "[00:10.00]<00:10.00>你好 <00:10.50>世界";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "你好 世界");
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[0].timestamp_ms, 10_000);
        assert_eq!(lines[0].words[0].text, "你好 ");
        assert_eq!(lines[0].words[1].timestamp_ms, 10_500);
        assert_eq!(lines[0].words[1].text, "世界");
    }

    #[test]
    fn lrc_offset_applies_to_enhanced_word_timing() {
        let lines = parse_lrc("[offset:-250]\n[00:10.00]<00:10.00>A<00:10.50>B");
        assert_eq!(lines[0].timestamp_ms, 9_750);
        assert_eq!(lines[0].words[0].timestamp_ms, 9_750);
        assert_eq!(lines[0].words[1].timestamp_ms, 10_250);
    }

    #[test]
    fn repeated_outer_timestamps_do_not_guess_word_timeline() {
        let lines = parse_lrc("[00:10.00][00:20.00]<00:10.00>重复句");
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.words.is_empty()));
    }

    #[test]
    fn decode_bytes_detects_gbk() {
        // "你好" in GBK is [0xC4, 0xE3, 0xBA, 0xC3]
        let gbk_bytes = [0xC4, 0xE3, 0xBA, 0xC3];
        let decoded = decode_bytes_to_string(&gbk_bytes);
        assert_eq!(decoded, "你好");
    }

    #[test]
    fn translated_lrc_is_paired_on_the_same_timeline() {
        let document = LyricsDocument::from_sources(
            None,
            Some("[00:01.00]Hello\n[00:02.00]Goodbye".into()),
            Some("[00:01.08]你好\n[00:02.12]再见".into()),
            "测试",
        );
        assert_eq!(document.timed_lines().len(), 2);
        assert_eq!(document.timed_lines()[0].text, "Hello");
        assert_eq!(
            document.timed_lines()[0].translation.as_deref(),
            Some("你好")
        );
        assert_eq!(document.timed_lines()[1].text, "Goodbye");
        assert_eq!(
            document.timed_lines()[1].translation.as_deref(),
            Some("再见")
        );
        assert!(document.has_translation());
    }

    #[test]
    fn equal_length_translation_survives_provider_timestamp_drift() {
        let document = LyricsDocument::from_sources(
            None,
            Some("[00:10.00]Hello\n[00:20.00]Goodbye".into()),
            Some("[00:12.50]你好\n[00:22.50]再见".into()),
            "测试",
        );
        assert_eq!(document.timed_lines().len(), 2);
        assert_eq!(document.timed_lines()[0].translation.as_deref(), Some("你好"));
        assert_eq!(document.timed_lines()[1].translation.as_deref(), Some("再见"));
        assert!(document.has_translation());
    }

    #[test]
    fn unmatched_translation_does_not_become_primary_lyric_row() {
        let document = LyricsDocument::from_sources(
            None,
            Some("[00:10.00]Hello".into()),
            Some("[00:10.10]你好\n[00:30.00]额外字幕".into()),
            "测试",
        );
        assert_eq!(document.timed_lines().len(), 1);
        assert_eq!(document.timed_lines()[0].text, "Hello");
        assert_eq!(document.timed_lines()[0].translation.as_deref(), Some("你好"));
    }

    #[test]
    fn legacy_merged_provider_lyrics_restore_translation() {
        let document = LyricsDocument::from_sources(
            None,
            Some("[00:01.00]Hello\n[00:01.00]你好".into()),
            None,
            "网易云音乐",
        );
        assert_eq!(document.timed_lines().len(), 1);
        assert_eq!(document.timed_lines()[0].text, "Hello");
        assert_eq!(
            document.timed_lines()[0].translation.as_deref(),
            Some("你好")
        );
    }
}
