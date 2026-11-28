//! 歌词格式互转：LRC ⇄ TTML。
//!
//! 编辑器内部统一用 LRC 文本作为「工作格式」，导入时把 TTML 拆成带时间标签的行，
//! 导出时再把 LRC 行重新组装成 TTML。只依赖标准库，方便单测。

/// 一条带时间戳的歌词。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    /// 起始时间，毫秒。
    pub time_ms: u64,
    /// 该时间点显示的文本；LRC 里允许是空串（间奏）。
    pub text: String,
}

/// 解析 LRC 文本，返回按时间升序排列的歌词行。
///
/// 支持一行多个时间标签（`[00:10.00][01:20.00]副歌`）与 `[mm:ss]` / `[mm:ss.x]`
/// / `[mm:ss.xx]` / `[mm:ss.xxx]` 几种时间精度；`[ar:]`、`[ti:]` 这类元数据标签
/// 与没有时间标签的普通文本会被忽略。
pub fn parse_lrc(src: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();

    for raw in src.lines() {
        let mut rest = raw.trim_start();
        let mut stamps = Vec::new();

        // 一行开头可能连着多个 `[..]`，只吞掉能解析成时间的那部分。
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(close) = stripped.find(']') else {
                break;
            };
            let tag = &stripped[..close];
            let Some(ms) = parse_timestamp(tag) else {
                break;
            };
            stamps.push(ms);
            rest = &stripped[close + 1..];
        }

        if stamps.is_empty() {
            continue;
        }

        let text = rest.trim().to_string();
        for time_ms in stamps {
            lines.push(LyricLine {
                time_ms,
                text: text.clone(),
            });
        }
    }

    lines.sort_by_key(|line| line.time_ms);
    lines
}

/// 把歌词行写回 LRC 文本，每行以 `\n` 结尾。
pub fn to_lrc(lines: &[LyricLine]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(&format_timestamp(line.time_ms));
        out.push_str(&line.text);
        out.push('\n');
    }
    out
}

/// 把 LRC 文本转成 TTML；每句的结束时间取下一句的开始时间。
pub fn to_ttml(lrc: &str) -> String {
    let lines = parse_lrc(lrc);
    let mut out = String::with_capacity(lrc.len() * 2 + 256);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(
        "<tt xmlns=\"http://www.w3.org/ns/ttml\" \
         xmlns:tts=\"http://www.w3.org/ns/ttml#styling\" xml:lang=\"zh-Hans\">\n",
    );
    out.push_str("  <head/>\n");
    out.push_str("  <body>\n    <div>\n");

    for (index, line) in lines.iter().enumerate() {
        // 结束时间：下一句的开始时间；最后一句给 4 秒，最短 0.5 秒。
        let end = match lines.get(index + 1) {
            Some(next) => next.time_ms.max(line.time_ms + MIN_LINE_MS),
            None => line.time_ms + LAST_LINE_MS,
        };
        out.push_str(&format!(
            "      <p begin=\"{}\" end=\"{}\">{}</p>\n",
            format_clock(line.time_ms),
            format_clock(end),
            escape_xml(&line.text),
        ));
    }

    out.push_str("    </div>\n  </body>\n</tt>\n");
    out
}

/// 把 TTML 文本拆回 LRC。只认 `<p begin="...">文本</p>`，
/// 行内的 `<span>`（逐字时间）会被剥掉，只保留文字。
pub fn from_ttml(src: &str) -> String {
    let mut lines: Vec<LyricLine> = Vec::new();
    let mut rest = src;

    while let Some(start) = rest.find("<p") {
        // `<p` 后面必须跟空白或 `>`，避免把 `<path>` 之类的标签认进来。
        let after = &rest[start + 2..];
        let boundary = after.starts_with(|c: char| c.is_whitespace() || c == '>');
        let Some(tag_end) = after.find('>') else {
            break;
        };
        if !boundary {
            rest = &after[tag_end + 1..];
            continue;
        }

        let head = &after[..tag_end];
        let body = &after[tag_end + 1..];
        let Some(close) = body.find("</p>") else {
            break;
        };
        let text = strip_tags(&body[..close]);

        if let Some(ms) = attr(head, "begin").and_then(parse_clock) {
            lines.push(LyricLine { time_ms: ms, text });
        }

        rest = &body[close + 4..];
    }

    lines.sort_by_key(|line| line.time_ms);
    to_lrc(&lines)
}

/// 导入任意歌词文本：认出 TTML 就转成 LRC，否则按 LRC 原样保留。
pub fn import_text(raw: &str) -> String {
    // 统一换行，避免 Windows 导出文件在编辑器里出现 `\r`。
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    if looks_like_ttml(&normalized) {
        let converted = from_ttml(&normalized);
        if !converted.trim().is_empty() {
            return converted;
        }
    }
    normalized
}

/// 粗判一段文本是不是 TTML。
pub fn looks_like_ttml(raw: &str) -> bool {
    let head = raw.trim_start();
    head.starts_with("<?xml") || head.starts_with("<tt")
}

/// 把文件名换成指定扩展名，供「另存为」的默认名字使用。
pub fn with_extension(name: &str, ext: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => format!("{stem}.{ext}"),
        _ => format!("{name}.{ext}"),
    }
}

/// 最短的单句展示时长（毫秒）。
const MIN_LINE_MS: u64 = 500;
/// 最后一句默认展示时长（毫秒）。
const LAST_LINE_MS: u64 = 4_000;

/// 解析表格里手写的时间（`mm:ss.xx`，可带方括号），返回毫秒。
pub fn parse_time(value: &str) -> Option<u64> {
    let value = value.trim().trim_matches(|c| c == '[' || c == ']');
    parse_timestamp(value)
}

/// 解析 `mm:ss.xx` 形式的时间标签，返回毫秒。
fn parse_timestamp(tag: &str) -> Option<u64> {
    let (minutes, rest) = tag.split_once(':')?;
    let minutes: u64 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = match rest.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (rest, None),
    };
    let seconds: u64 = seconds.trim().parse().ok()?;

    let ms = match fraction {
        Some(fraction) => {
            let digits: String = fraction.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                0
            } else {
                // 1 位 = 100ms，2 位 = 10ms，3 位起按毫秒截断。
                let mut padded = digits;
                padded.truncate(3);
                while padded.len() < 3 {
                    padded.push('0');
                }
                padded.parse().ok()?
            }
        }
        None => 0,
    };

    Some((minutes * 60 + seconds) * 1000 + ms)
}

/// 渲染成 LRC 的 `[mm:ss.xx]`。
fn format_timestamp(ms: u64) -> String {
    format!("[{}]", format_time(ms))
}

/// 渲染成 `mm:ss.xx`（不带方括号），供表格的时间列使用。
pub fn format_time(ms: u64) -> String {
    format!(
        "{:02}:{:02}.{:02}",
        ms / 60_000,
        (ms / 1_000) % 60,
        (ms % 1_000) / 10
    )
}

/// 渲染成 TTML 的 `hh:mm:ss.mmm`。
fn format_clock(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1_000) % 60,
        ms % 1_000
    )
}

/// 解析 TTML 的时间写法：`hh:mm:ss.mmm`、`mm:ss.mmm`，或带单位的 `12.5s` / `500ms`。
fn parse_clock(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if let Some(rest) = value.strip_suffix("ms") {
        return rest.trim().parse().ok();
    }
    if let Some(rest) = value.strip_suffix('s') {
        return rest.trim().parse::<f64>().ok().map(|s| (s * 1000.) as u64);
    }

    let mut milliseconds = 0.;
    let mut scale = 1000_f64; // 从「秒」开始往左变大
    // 从右往左按 `:` 切，先处理最右边的秒（可能带小数）。
    for (index, part) in value.rsplit(':').enumerate() {
        let number: f64 = part.trim().parse().ok()?;
        milliseconds += number * scale;
        scale *= 60.;
        if index == 2 {
            break;
        }
    }

    Some(milliseconds as u64)
}

/// 取标签里的属性值。
fn attr<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    let pattern = format!("{name}=\"");
    let start = head.find(&pattern)? + pattern.len();
    let rest = &head[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// 去掉行内标签，只留文字。
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0;
    for ch in text.chars() {
        match ch {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    unescape_xml(out.trim())
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LRC: &str = "[00:01.00]第一句\n[00:05.50]第二句\n[00:09.20]第三句\n";

    #[test]
    fn parses_timestamps_and_text() {
        let lines = parse_lrc(LRC);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].time_ms, 1_000);
        assert_eq!(lines[1].time_ms, 5_500);
        assert_eq!(lines[1].text, "第二句");
    }

    #[test]
    fn keeps_multiple_tags_on_one_line() {
        let lines = parse_lrc("[00:10.00][01:20.00]副歌\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time_ms, 10_000);
        assert_eq!(lines[1].time_ms, 80_000);
        assert!(lines.iter().all(|line| line.text == "副歌"));
    }

    #[test]
    fn skips_metadata_and_plain_text() {
        let lines = parse_lrc("[ar:某人]\n[ti:某歌]\n随便一行\n[00:02.000]正文\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].time_ms, 2_000);
    }

    #[test]
    fn sorts_by_time() {
        let lines = parse_lrc("[00:20.00]后\n[00:05.00]前\n");
        assert_eq!(lines[0].text, "前");
        assert_eq!(lines[1].text, "后");
    }

    #[test]
    fn renders_lrc_round_trip() {
        let text = to_lrc(&parse_lrc(LRC));
        assert_eq!(text, LRC);
    }

    #[test]
    fn writes_ttml_with_begin_and_end() {
        let ttml = to_ttml(LRC);
        assert!(ttml.starts_with("<?xml"));
        assert!(ttml.contains("<p begin=\"00:00:01.000\" end=\"00:00:05.500\">第一句</p>"));
        // 最后一句按 4 秒补齐。
        assert!(ttml.contains("begin=\"00:00:09.200\" end=\"00:00:13.200\""));
    }

    #[test]
    fn escapes_and_unescapes_xml() {
        let ttml = to_ttml("[00:01.00]A & B < C\n");
        assert!(ttml.contains("A &amp; B &lt; C"));
        let back = from_ttml(&ttml);
        assert_eq!(back, "[00:01.00]A & B < C\n");
    }

    #[test]
    fn ttml_round_trip_drops_spans() {
        let ttml = "<?xml version=\"1.0\"?><tt><body><div>\
            <p begin=\"00:00:02.000\"><span begin=\"00:00:02.000\">你</span>好</p>\
            <p begin=\"00:00:06.500\">再见</p>\
            </div></body></tt>";
        let lrc = from_ttml(ttml);
        assert_eq!(lrc, "[00:02.00]你好\n[00:06.50]再见\n");
    }

    #[test]
    fn parses_clock_variants() {
        assert_eq!(parse_clock("00:00:01.250"), Some(1_250));
        assert_eq!(parse_clock("01:02.5"), Some(62_500));
        assert_eq!(parse_clock("2.5s"), Some(2_500));
        assert_eq!(parse_clock("750ms"), Some(750));
        assert_eq!(parse_clock("oops"), None);
    }

    #[test]
    fn detects_ttml() {
        assert!(looks_like_ttml("  <?xml version=\"1.0\"?><tt/>"));
        assert!(looks_like_ttml("<tt xmlns=\"...\"></tt>"));
        assert!(!looks_like_ttml("[00:01.00]歌词"));
    }

    #[test]
    fn import_normalizes_newlines_and_converts_ttml() {
        let raw = "[00:01.00]第一句\r\n[00:02.00]第二句\r\n";
        assert_eq!(import_text(raw), "[00:01.00]第一句\n[00:02.00]第二句\n");
        let ttml = to_ttml("[00:01.00]第一句\n");
        assert_eq!(import_text(&ttml), "[00:01.00]第一句\n");
    }

    #[test]
    fn swaps_extension() {
        assert_eq!(with_extension("song.lrc", "ttml"), "song.ttml");
        assert_eq!(with_extension("song", "lrc"), "song.lrc");
        assert_eq!(with_extension(".hidden", "lrc"), ".hidden.lrc");
    }
}
