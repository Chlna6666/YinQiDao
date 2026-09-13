use std::{collections::HashMap, sync::Arc};

use super::{LyricLine, LyricWord, parse_lrc};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SyncedLyricsFormat {
    Lrc,
    Qrc,
    Yrc,
    Ttml,
}

pub(super) fn detect_synced_format(input: &str) -> SyncedLyricsFormat {
    let trimmed = input.trim_start_matches('\u{feff}').trim_start();
    if looks_like_ttml(trimmed) {
        return SyncedLyricsFormat::Ttml;
    }
    if looks_like_qrc_xml(trimmed) {
        return SyncedLyricsFormat::Qrc;
    }
    for line in trimmed.lines() {
        let Some((_, _, content)) = parse_millisecond_line_header(line) else {
            continue;
        };
        let content = content.trim_start();
        if content.starts_with('(')
            && content
                .get(1..)
                .and_then(|rest| rest.find(')'))
                .is_some_and(|end| parse_tuple::<3>(&content[1..1 + end]).is_some())
        {
            return SyncedLyricsFormat::Yrc;
        }
        return SyncedLyricsFormat::Qrc;
    }
    SyncedLyricsFormat::Lrc
}

pub(super) fn parse_synced_lyrics(input: &str) -> Vec<LyricLine> {
    match detect_synced_format(input) {
        SyncedLyricsFormat::Lrc => parse_lrc(input),
        SyncedLyricsFormat::Qrc => parse_qrc(input),
        SyncedLyricsFormat::Yrc => parse_yrc(input),
        SyncedLyricsFormat::Ttml => parse_ttml(input),
    }
}

fn looks_like_ttml(input: &str) -> bool {
    (input.starts_with("<?xml") && input.contains("<tt"))
        || input.contains("<tt ")
        || input.contains("<tt>")
        || input.contains("itunes:timing=")
        || (input.contains("<p ") && input.contains(" begin="))
}

fn looks_like_qrc_xml(input: &str) -> bool {
    input.contains("<QrcInfos")
        || input.contains("<LyricInfo")
        || input.contains("LyricContent=")
}

fn parse_qrc(input: &str) -> Vec<LyricLine> {
    let decoded;
    let payload = if looks_like_qrc_xml(input) {
        let Some(raw) = extract_xml_attribute(input, "LyricContent") else {
            return Vec::new();
        };
        decoded = decode_xml_entities(raw);
        decoded.as_str()
    } else {
        input
    };

    let mut lines = Vec::new();
    for raw_line in payload.lines() {
        let Some((line_start, _line_duration, content)) = parse_millisecond_line_header(raw_line)
        else {
            continue;
        };
        let (text, words) = parse_qrc_words(content);
        if text.trim().is_empty() {
            continue;
        }
        lines.push(LyricLine {
            timestamp_ms: line_start,
            text,
            translation: None,
            words: words.into(),
        });
    }
    lines.sort_by_key(|line| line.timestamp_ms);
    lines
}

fn parse_yrc(input: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw_line in input.lines() {
        let Some((line_start, _line_duration, content)) = parse_millisecond_line_header(raw_line)
        else {
            continue;
        };
        let (text, words) = parse_yrc_words(content);
        if text.trim().is_empty() {
            continue;
        }
        lines.push(LyricLine {
            timestamp_ms: line_start,
            text,
            translation: None,
            words: words.into(),
        });
    }
    lines.sort_by_key(|line| line.timestamp_ms);
    lines
}

fn parse_millisecond_line_header(line: &str) -> Option<(u64, u64, &str)> {
    let line = line.trim_start();
    let rest = line.strip_prefix('[')?;
    let end = rest.find(']')?;
    let [start, duration] = parse_tuple::<2>(&rest[..end])?;
    Some((start, duration, &rest[end + 1..]))
}

fn parse_tuple<const N: usize>(value: &str) -> Option<[u64; N]> {
    let mut parts = value.split(',');
    let mut values = [0_u64; N];
    for slot in &mut values {
        *slot = parts.next()?.trim().parse::<u64>().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(values)
}

fn parse_qrc_words(content: &str) -> (String, Vec<LyricWord>) {
    let mut text = String::with_capacity(content.len());
    let mut words = Vec::new();
    let mut cursor = 0usize;

    while cursor < content.len() {
        let Some(relative_open) = content[cursor..].find('(') else {
            text.push_str(&content[cursor..]);
            break;
        };
        let open = cursor + relative_open;
        let Some(relative_close) = content[open + 1..].find(')') else {
            text.push_str(&content[cursor..]);
            break;
        };
        let close = open + 1 + relative_close;
        let Some([start, duration]) = parse_tuple::<2>(&content[open + 1..close]) else {
            text.push_str(&content[cursor..=open]);
            cursor = open + 1;
            continue;
        };

        let segment = &content[cursor..open];
        text.push_str(segment);
        if !segment.is_empty() && duration > 0 {
            words.push(LyricWord {
                timestamp_ms: start,
                text: segment.to_owned(),
            });
        }
        cursor = close + 1;
    }

    (text, words)
}

fn parse_yrc_words(content: &str) -> (String, Vec<LyricWord>) {
    let mut text = String::with_capacity(content.len());
    let mut words = Vec::new();
    let mut cursor = 0usize;

    while cursor < content.len() {
        let Some(relative_open) = content[cursor..].find('(') else {
            text.push_str(&content[cursor..]);
            break;
        };
        let open = cursor + relative_open;
        if open > cursor {
            text.push_str(&content[cursor..open]);
        }
        let Some(relative_close) = content[open + 1..].find(')') else {
            text.push_str(&content[open..]);
            break;
        };
        let close = open + 1 + relative_close;
        let Some([start, duration, _reserved]) = parse_tuple::<3>(&content[open + 1..close]) else {
            text.push('(');
            cursor = open + 1;
            continue;
        };
        let segment_start = close + 1;
        let segment_end = content[segment_start..]
            .find('(')
            .map_or(content.len(), |relative| segment_start + relative);
        let segment = &content[segment_start..segment_end];
        text.push_str(segment);
        if !segment.is_empty() && duration > 0 {
            words.push(LyricWord {
                timestamp_ms: start,
                text: segment.to_owned(),
            });
        }
        cursor = segment_end;
    }

    (text, words)
}

fn parse_ttml(input: &str) -> Vec<LyricLine> {
    let metadata_translations = parse_ttml_metadata_translations(input);
    let mut lines = Vec::new();
    let mut cursor = 0usize;

    while let Some(open) = find_open_tag(input, cursor, "p") {
        let Some(close) = find_matching_close(input, open.end, "p") else {
            break;
        };
        let inner = &input[open.end..close.start];
        let key = xml_attr(open.attrs, "key").map(str::to_owned);
        let inline_translation = find_role_text(inner, "x-translation");
        let words = parse_ttml_words(inner);
        let text = if words.is_empty() {
            xml_text_excluding_auxiliary(inner)
        } else {
            words.iter().map(|word| word.text.as_str()).collect::<String>()
        };
        if text.trim().is_empty() {
            cursor = close.end;
            continue;
        }

        let timestamp_ms = xml_attr(open.attrs, "begin")
            .and_then(parse_ttml_time)
            .or_else(|| words.first().map(|word| word.timestamp_ms));
        let Some(timestamp_ms) = timestamp_ms else {
            cursor = close.end;
            continue;
        };
        let translation = inline_translation.or_else(|| {
            key.as_deref()
                .and_then(|key| metadata_translations.get(key).cloned())
        });
        lines.push(LyricLine {
            timestamp_ms,
            text,
            translation,
            words: words.into(),
        });
        cursor = close.end;
    }

    lines.sort_by_key(|line| line.timestamp_ms);
    lines
}

fn parse_ttml_words(input: &str) -> Vec<LyricWord> {
    let mut words = Vec::new();
    let mut cursor = 0usize;
    while let Some(open) = find_open_tag(input, cursor, "span") {
        let Some(close) = find_matching_close(input, open.end, "span") else {
            break;
        };
        let role = xml_attr(open.attrs, "role").unwrap_or_default();
        let ruby = xml_attr(open.attrs, "ruby").unwrap_or_default();
        let excluded = matches!(role, "x-translation" | "x-roman" | "x-bg")
            || matches!(ruby, "text" | "textContainer");
        if !excluded
            && let Some(timestamp_ms) = xml_attr(open.attrs, "begin").and_then(parse_ttml_time)
        {
            let text = xml_text_excluding_auxiliary(&input[open.end..close.start]);
            if !text.is_empty() {
                words.push(LyricWord { timestamp_ms, text });
            }
        }
        cursor = close.end;
    }
    words.sort_by_key(|word| word.timestamp_ms);
    words
}

fn parse_ttml_metadata_translations(input: &str) -> HashMap<String, String> {
    let mut translations = HashMap::new();
    let mut cursor = 0usize;
    while let Some(open) = find_open_tag(input, cursor, "text") {
        let Some(close) = find_matching_close(input, open.end, "text") else {
            break;
        };
        if let Some(key) = xml_attr(open.attrs, "for") {
            let value = xml_text_excluding_auxiliary(&input[open.end..close.start]);
            if !value.is_empty() {
                translations.entry(key.to_owned()).or_insert(value);
            }
        }
        cursor = close.end;
    }
    translations
}

fn find_role_text(input: &str, wanted_role: &str) -> Option<String> {
    let mut cursor = 0usize;
    while let Some(open) = find_open_tag(input, cursor, "span") {
        let close = find_matching_close(input, open.end, "span")?;
        if xml_attr(open.attrs, "role") == Some(wanted_role) {
            let text = xml_text_excluding_auxiliary(&input[open.end..close.start]);
            if !text.is_empty() {
                return Some(text);
            }
        }
        cursor = close.end;
    }
    None
}

#[derive(Clone, Copy)]
struct XmlTag<'a> {
    start: usize,
    end: usize,
    attrs: &'a str,
}

fn find_open_tag<'a>(input: &'a str, mut cursor: usize, local_name: &str) -> Option<XmlTag<'a>> {
    while cursor < input.len() {
        let relative = input[cursor..].find('<')?;
        let start = cursor + relative;
        let close = input[start + 1..].find('>')? + start + 1;
        let header = input[start + 1..close].trim();
        if header.starts_with('/') || header.starts_with('!') || header.starts_with('?') {
            cursor = close + 1;
            continue;
        }
        let name_end = header
            .find(|ch: char| ch.is_whitespace() || ch == '/')
            .unwrap_or(header.len());
        let name = &header[..name_end];
        if xml_local_name(name) == local_name {
            return Some(XmlTag {
                start,
                end: close + 1,
                attrs: header[name_end..].trim(),
            });
        }
        cursor = close + 1;
    }
    None
}

fn find_matching_close<'a>(
    input: &'a str,
    mut cursor: usize,
    local_name: &str,
) -> Option<XmlTag<'a>> {
    let mut depth = 1usize;
    while cursor < input.len() {
        let relative = input[cursor..].find('<')?;
        let start = cursor + relative;
        let close = input[start + 1..].find('>')? + start + 1;
        let header = input[start + 1..close].trim();
        if header.starts_with('!') || header.starts_with('?') {
            cursor = close + 1;
            continue;
        }
        let closing = header.starts_with('/');
        let body = header.trim_start_matches('/').trim_start();
        let name_end = body
            .find(|ch: char| ch.is_whitespace() || ch == '/')
            .unwrap_or(body.len());
        let name = &body[..name_end];
        if xml_local_name(name) == local_name {
            if closing {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(XmlTag {
                        start,
                        end: close + 1,
                        attrs: "",
                    });
                }
            } else if !header.ends_with('/') {
                depth += 1;
            }
        }
        cursor = close + 1;
    }
    None
}

#[inline]
fn xml_local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn xml_attr<'a>(attrs: &'a str, wanted_local_name: &str) -> Option<&'a str> {
    let bytes = attrs.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && bytes[cursor] != b'='
            && bytes[cursor] != b'/'
        {
            cursor += 1;
        }
        if cursor == name_start {
            cursor += 1;
            continue;
        }
        let name = &attrs[name_start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let quote = *bytes.get(cursor)?;
        if quote != b'\'' && quote != b'"' {
            cursor += 1;
            continue;
        }
        cursor += 1;
        let value_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != quote {
            cursor += 1;
        }
        let value = &attrs[value_start..cursor];
        cursor = cursor.saturating_add(1);
        if xml_local_name(name) == wanted_local_name {
            return Some(value);
        }
    }
    None
}

fn extract_xml_attribute<'a>(input: &'a str, wanted: &str) -> Option<&'a str> {
    let mut cursor = 0usize;
    while let Some(relative) = input[cursor..].find(wanted) {
        let start = cursor + relative;
        let after_name = start + wanted.len();
        let tail = input.get(after_name..)?;
        let equal = tail.find('=')? + after_name;
        if input[after_name..equal].chars().all(char::is_whitespace) {
            let bytes = input.as_bytes();
            let mut value_start = equal + 1;
            while value_start < bytes.len() && bytes[value_start].is_ascii_whitespace() {
                value_start += 1;
            }
            let quote = *bytes.get(value_start)?;
            if quote == b'\'' || quote == b'"' {
                value_start += 1;
                let end = input[value_start..].find(quote as char)? + value_start;
                return Some(&input[value_start..end]);
            }
        }
        cursor = after_name;
    }
    None
}

fn xml_text_excluding_auxiliary(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut cursor = 0usize;
    while cursor < input.len() {
        let Some(relative) = input[cursor..].find('<') else {
            append_decoded_xml_entities(&mut result, &input[cursor..]);
            break;
        };
        let tag_start = cursor + relative;
        append_decoded_xml_entities(&mut result, &input[cursor..tag_start]);
        let Some(close) = input[tag_start + 1..].find('>') else {
            append_decoded_xml_entities(&mut result, &input[tag_start..]);
            break;
        };
        let tag_end = tag_start + 1 + close;
        let header = input[tag_start + 1..tag_end].trim();
        if !header.starts_with('/') && !header.starts_with('!') && !header.starts_with('?') {
            let name_end = header
                .find(|ch: char| ch.is_whitespace() || ch == '/')
                .unwrap_or(header.len());
            let name = &header[..name_end];
            let attrs = header[name_end..].trim();
            if xml_local_name(name) == "span" {
                let role = xml_attr(attrs, "role").unwrap_or_default();
                let ruby = xml_attr(attrs, "ruby").unwrap_or_default();
                let skip = matches!(role, "x-translation" | "x-roman" | "x-bg")
                    || matches!(ruby, "text" | "textContainer");
                if skip
                    && let Some(matching) = find_matching_close(input, tag_end + 1, "span")
                {
                    cursor = matching.end;
                    continue;
                }
            }
        }
        cursor = tag_end + 1;
    }

    let leading = result.len() - result.trim_start().len();
    let trimmed_len = result.trim().len();
    if leading > 0 {
        result.drain(..leading);
    }
    result.truncate(trimmed_len);
    result
}

fn decode_xml_entities(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    append_decoded_xml_entities(&mut output, input);
    output
}

fn append_decoded_xml_entities(output: &mut String, input: &str) {
    if !input.contains('&') {
        output.push_str(input);
        return;
    }

    let mut cursor = 0usize;
    while cursor < input.len() {
        let Some(relative_amp) = input[cursor..].find('&') else {
            output.push_str(&input[cursor..]);
            break;
        };
        let amp = cursor + relative_amp;
        output.push_str(&input[cursor..amp]);
        let Some(relative_end) = input[amp + 1..].find(';') else {
            output.push_str(&input[amp..]);
            break;
        };
        let end = amp + 1 + relative_end;
        let entity = &input[amp + 1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") => u32::from_str_radix(&entity[2..], 16)
                .ok()
                .and_then(char::from_u32),
            _ if entity.starts_with('#') => entity[1..]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32),
            _ => None,
        };
        if let Some(ch) = decoded {
            output.push(ch);
        } else {
            output.push_str(&input[amp..=end]);
        }
        cursor = end + 1;
    }
}

fn parse_ttml_time(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Some(raw) = value.strip_suffix("ms") {
        return parse_non_negative_f64(raw).map(|value| value.round() as u64);
    }
    if let Some(raw) = value.strip_suffix('s') {
        return parse_non_negative_f64(raw).map(|value| (value * 1_000.0).round() as u64);
    }
    if let Some(raw) = value.strip_suffix('m') {
        return parse_non_negative_f64(raw).map(|value| (value * 60_000.0).round() as u64);
    }

    let seconds = if let Some((first, rest)) = value.split_once(':') {
        if let Some((second, third)) = rest.split_once(':') {
            if third.contains(':') {
                return None;
            }
            first.parse::<u64>().ok()? as f64 * 3_600.0
                + second.parse::<u64>().ok()? as f64 * 60.0
                + parse_non_negative_f64(third)?
        } else {
            first.parse::<u64>().ok()? as f64 * 60.0 + parse_non_negative_f64(rest)?
        }
    } else {
        parse_non_negative_f64(value)?
    };
    Some((seconds * 1_000.0).round() as u64)
}

fn parse_non_negative_f64(value: &str) -> Option<f64> {
    let parsed = value.trim().parse::<f64>().ok()?;
    parsed.is_finite().then_some(parsed.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_parses_qrc_word_timing() {
        let input = "[750,1330]A (750,180)Sky (930,180)Full(1110,150)";
        assert_eq!(detect_synced_format(input), SyncedLyricsFormat::Qrc);
        let lines = parse_synced_lyrics(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].timestamp_ms, 750);
        assert_eq!(lines[0].text, "A Sky Full");
        assert_eq!(lines[0].words.len(), 3);
        assert_eq!(lines[0].words[1].timestamp_ms, 930);
        assert_eq!(lines[0].words[1].text, "Sky ");
    }

    #[test]
    fn parses_qrc_xml_lyric_content() {
        let input = r#"<QrcInfos><LyricInfo><Lyric_1 LyricType="1" LyricContent="[1000,800]你(1000,300)好(1300,500)"/></LyricInfo></QrcInfos>"#;
        let lines = parse_synced_lyrics(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "你好");
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[1].timestamp_ms, 1_300);
    }

    #[test]
    fn detects_and_parses_yrc_word_timing() {
        let input = "[54260,3090](54260,900,0)Stop (55160,480,0)and (55640,1710,0)stare";
        assert_eq!(detect_synced_format(input), SyncedLyricsFormat::Yrc);
        let lines = parse_synced_lyrics(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Stop and stare");
        assert_eq!(lines[0].words.len(), 3);
        assert_eq!(lines[0].words[2].timestamp_ms, 55_640);
    }

    #[test]
    fn parses_ttml_words_and_inline_translation() {
        let input = r#"<?xml version="1.0"?><tt xmlns:ttm="http://www.w3.org/ns/ttml#metadata"><body><div><p begin="00:10.000" end="00:12.000" itunes:key="L1"><span begin="00:10.000" end="00:10.500">你</span><span begin="00:10.500" end="00:12.000">好</span><span ttm:role="x-translation" xml:lang="en">Hello</span></p></div></body></tt>"#;
        assert_eq!(detect_synced_format(input), SyncedLyricsFormat::Ttml);
        let lines = parse_synced_lyrics(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].timestamp_ms, 10_000);
        assert_eq!(lines[0].text, "你好");
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].translation.as_deref(), Some("Hello"));
    }

    #[test]
    fn parses_apple_metadata_translation() {
        let input = r#"<tt><head><metadata><iTunesMetadata><translations><translation><text for="L1">你好世界</text></translation></translations></iTunesMetadata></metadata></head><body><div><p begin="12.3s" itunes:key="L1">Hello world</p></div></body></tt>"#;
        let lines = parse_synced_lyrics(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].timestamp_ms, 12_300);
        assert_eq!(lines[0].text, "Hello world");
        assert_eq!(lines[0].translation.as_deref(), Some("你好世界"));
    }

    #[test]
    fn decodes_xml_entities_in_place() {
        assert_eq!(
            decode_xml_entities("A &amp; B &#x4F60;&#22909; &unknown;"),
            "A & B 你好 &unknown;"
        );
        assert_eq!(
            xml_text_excluding_auxiliary("  <span>Hi &amp; 你好</span>  "),
            "Hi & 你好"
        );
    }

    #[test]
    fn falls_back_to_lrc() {
        let input = "[00:01.00]Hello";
        assert_eq!(detect_synced_format(input), SyncedLyricsFormat::Lrc);
        assert_eq!(parse_synced_lyrics(input)[0].timestamp_ms, 1_000);
    }
}
