//! 洛雪（LX Music）音源接入。
//!
//! 洛雪的「音源」分两层，这里接的是它们各自能提供的东西：
//!
//! - **搜索层**：洛雪客户端内置的各平台 SDK（`musicSdk/{kw,kg,tx,wy,mg}`），
//!   既不在自定义源脚本里，也不在服务端接口里。我们内置了咪咕搜索（相当于 `mg` 那套），
//!   官方曲库、结果最准，还直接给 LRC。
//! - **歌词 / 直链层**：`lx-music-api-server`（`FIREFLY_LX_API` 指向它）提供
//!   `/lyric?source=&songId=`、`/url?source=&songId=&quality=`、`/info?source=&songId=`。
//!   注意它**没有搜索接口**，且只覆盖 kw / kg / tx / wy（不含 mg），所以只有当手上
//!   拿着这几个平台的 songId 时才有用 —— 见 [`lx_server_lyric`]、[`song_url`]。
//!
//! 兜底还有网易云公开接口（实测搜不到正版曲库，只当最后一道保险）。
//!
//! 所有通路都收敛成 [`MatchedTrack`]，上层只认这个结构。

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::Value;

/// 请求音源接口用的 UA（网易云对空 UA 会直接拒）。
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
                         (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
/// 单次请求超时。
const TIMEOUT: Duration = Duration::from_secs(8);
/// 一次拉多少条候选。
const SEARCH_LIMIT: usize = 20;
/// 咪咕标签里能认出来的流派。
const KNOWN_GENRES: [&str; 12] = [
    "流行",
    "摇滚",
    "民谣",
    "电子",
    "说唱",
    "嘻哈",
    "R&B",
    "爵士",
    "古典",
    "金属",
    "乡村",
    "轻音乐",
];

/// 一个匹配成功的曲目：要填进表单的字段 + 歌词。
#[derive(Clone, Debug, Default)]
pub struct MatchedTrack {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: String,
    pub genre: String,
    pub track_no: String,
    /// LRC 文本；纯音乐等拿不到时是 `None`。
    pub lyric: Option<String>,
    /// 封面图地址（音源给了才有）。
    pub cover_url: Option<String>,
    /// 数据来自哪个音源，用于界面提示。
    pub provider: String,
}

/// 音源里的一条候选。
#[derive(Clone, Debug)]
struct Candidate {
    id: String,
    title: String,
    artist: String,
    album: String,
    year: String,
    /// 流派（咪咕会从标签里给）。
    genre: String,
    /// 歌词直链（咪咕直接给 LRC 地址）。
    lyric_url: Option<String>,
    /// 封面图地址。
    cover_url: Option<String>,
}

/// 按「标题 + 艺术家」搜一首歌，并把歌词一起取回来。
///
/// 这是给界面用的唯一入口，内部会阻塞（`ureq` 同步请求），要放在后台线程跑。
pub fn match_track(title: &str, artist: Option<&str>) -> Result<MatchedTrack, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("先填写歌曲标题，再点匹配".into());
    }
    let artist = artist.map(str::trim).filter(|artist| !artist.is_empty());

    if let Ok(track) = match_via_migu(title, artist) {
        return Ok(track);
    }
    match_via_netease(title, artist)
}

/// 把歌词写成音频文件同目录下的 `曲名.lrc`，返回写入路径。
pub fn write_lyric(audio_path: &Path, lyric: &str) -> Result<PathBuf, String> {
    let target = audio_path.with_extension("lrc");
    std::fs::write(&target, lyric).map_err(|error| format!("歌词写入失败：{error}"))?;
    Ok(target)
}

/// 洛雪 API 地址，来自环境变量，可选。
fn lx_api_base() -> Option<String> {
    std::env::var("FIREFLY_LX_API")
        .ok()
        .map(|base| base.trim().trim_end_matches('/').to_string())
        .filter(|base| !base.is_empty())
}

// ---- 洛雪服务端（lx-music-api-server）----
//
// 真实端点只有三个，参数名是 `songId`（不是 `id`）：
//   /lyric?source=&songId=
//   /url?source=&songId=&quality=
//   /info?source=&songId=
// 而且**没有 /search**（作者把搜索列为未实现），所以它只补歌词和直链，
// 不参与「按歌名搜索」——搜索走下面的咪咕。

/// 用洛雪服务端取歌词。`source` 支持 kw / kg / tx / wy（不含 mg）。
pub fn lx_server_lyric(source: &str, song_id: &str) -> Result<Option<String>, String> {
    let base = lx_api_base().ok_or_else(|| "没有配置 FIREFLY_LX_API".to_string())?;
    let url = format!("{base}/lyric?source={source}&songId={}", encode(song_id));
    let body = get_json(&url, None)?;
    Ok(parse_lyric(&body))
}

/// 用洛雪服务端取播放直链。播放器还没接，先把能力放这儿。
pub fn song_url(source: &str, song_id: &str, quality: &str) -> Result<String, String> {
    let base = lx_api_base().ok_or_else(|| "没有配置 FIREFLY_LX_API".to_string())?;
    let url = format!(
        "{base}/url?source={source}&songId={}&quality={}",
        encode(song_id),
        encode(quality)
    );
    let body = get_json(&url, None)?;
    body.pointer("/data/url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "洛雪服务端没有返回播放地址".to_string())
}

/// 歌词响应各版本略有差别：可能是 `data.lyric`、`lyric`，也可能 `lyric` 是个对象。
fn parse_lyric(body: &Value) -> Option<String> {
    let text = match body.pointer("/data/lyric").or_else(|| body.get("lyric")) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Object(object)) => object
            .get("lyric")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    };
    (!text.trim().is_empty()).then_some(text)
}

// ---- 内置咪咕音源（洛雪 `mg` 音源的数据源）----

fn match_via_migu(title: &str, artist: Option<&str>) -> Result<MatchedTrack, String> {
    let keyword = search_keyword(title, artist);
    // `searchSwitch={"song":1}` 已经预先百分号编码，避免大括号被拒。
    let url = format!(
        "https://app.c.nf.migu.cn/MIGUM2.0/v1.0/content/search_all.do?text={}&pageNo=1&pageSize={SEARCH_LIMIT}&searchSwitch=%7B%22song%22%3A1%7D",
        encode(&keyword)
    );
    let body = get_json(&url, None)?;
    let best = pick_best(migu_candidates(&body), title, artist)
        .ok_or_else(|| "咪咕音源里没有找到这首歌".to_string())?;

    // 咪咕直接给歌词文件地址，拉下来就是现成的 LRC。
    let lyric = best
        .lyric_url
        .as_deref()
        .and_then(|url| get_text(url, None).ok())
        .filter(|text| !text.trim().is_empty());

    Ok(MatchedTrack {
        title: best.title,
        artist: best.artist,
        album: best.album,
        year: best.year,
        genre: best.genre,
        track_no: String::new(),
        lyric,
        cover_url: best.cover_url,
        provider: "咪咕音源".into(),
    })
}

fn migu_candidates(body: &Value) -> Vec<Candidate> {
    body.pointer("/songResultData/result")
        .and_then(Value::as_array)
        .map(|songs| songs.iter().filter_map(migu_candidate).collect())
        .unwrap_or_default()
}

fn migu_candidate(song: &Value) -> Option<Candidate> {
    let id = song.get("id").and_then(Value::as_str)?.to_string();
    let title = song.get("name").and_then(Value::as_str)?.to_string();
    let artist = song
        .get("singers")
        .and_then(Value::as_array)
        .map(|singers| {
            singers
                .iter()
                .filter_map(|singer| singer.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default();
    let album = song
        .get("albums")
        .and_then(Value::as_array)
        .and_then(|albums| albums.first())
        .and_then(|album| album.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // 咪咕没有单独的流派字段，从标签里挑第一个能对上流派表的。
    let genre = song
        .get("tags")
        .and_then(Value::as_array)
        .and_then(|tags| {
            tags.iter().filter_map(Value::as_str).find(|tag| {
                KNOWN_GENRES
                    .iter()
                    .any(|genre| tag.eq_ignore_ascii_case(genre))
            })
        })
        .unwrap_or_default()
        .to_string();
    let lyric_url = song
        .get("lyricUrl")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    let cover_url = song
        .get("imgItems")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("img"))
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(Candidate {
        id,
        title,
        artist,
        album,
        year: String::new(),
        genre,
        lyric_url,
        cover_url,
    })
}

// ---- 内置网易云通路 ----

fn match_via_netease(title: &str, artist: Option<&str>) -> Result<MatchedTrack, String> {
    let keyword = search_keyword(title, artist);
    let url = format!(
        "https://music.163.com/api/search/get/web?s={}&type=1&limit={SEARCH_LIMIT}&offset=0",
        encode(&keyword)
    );
    let body = get_json(&url, Some("https://music.163.com/"))?;
    let best = pick_best(netease_candidates(&body), title, artist)
        .ok_or_else(|| "音源里没有找到这首歌".to_string())?;
    let lyric = netease_lyric(&best.id).unwrap_or_default();

    Ok(MatchedTrack {
        title: best.title,
        artist: best.artist,
        album: best.album,
        year: best.year,
        genre: String::new(),
        track_no: String::new(),
        lyric,
        cover_url: None,
        provider: "网易云音源".into(),
    })
}

fn netease_candidates(body: &Value) -> Vec<Candidate> {
    body.pointer("/result/songs")
        .and_then(Value::as_array)
        .map(|songs| songs.iter().filter_map(netease_candidate).collect())
        .unwrap_or_default()
}

fn netease_candidate(song: &Value) -> Option<Candidate> {
    let id = song.get("id").and_then(Value::as_i64)?.to_string();
    let title = song.get("name").and_then(Value::as_str)?.to_string();
    let artist = song
        .get("artists")
        .and_then(Value::as_array)
        .map(|artists| {
            artists
                .iter()
                .filter_map(|artist| artist.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default();
    let album = song.get("album");
    let album_name = album
        .and_then(|album| album.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let year = album
        .and_then(|album| album.get("publishTime"))
        .and_then(Value::as_i64)
        .map(year_of_millis)
        .unwrap_or_default();

    Some(Candidate {
        id,
        title,
        artist,
        album: album_name,
        year,
        genre: String::new(),
        lyric_url: None,
        cover_url: None,
    })
}

fn netease_lyric(id: &str) -> Result<Option<String>, String> {
    let url = format!("https://music.163.com/api/song/lyric?id={id}&lv=1&kv=1&tv=-1");
    let body = get_json(&url, Some("https://music.163.com/"))?;
    let lyric = body
        .pointer("/lrc/lyric")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((!lyric.trim().is_empty()).then_some(lyric))
}

// ---- 公共小工具 ----

/// 一次 HTTP GET，返回原始文本。
fn get_text(url: &str, referer: Option<&str>) -> Result<String, String> {
    let mut request = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .timeout(TIMEOUT);
    if let Some(referer) = referer {
        request = request.set("Referer", referer);
    }
    request
        .call()
        .map_err(|error| format!("请求音源失败：{error}"))?
        .into_string()
        .map_err(|error| format!("读取音源响应失败：{error}"))
}

/// 一次 HTTP GET + JSON 解析。
fn get_json(url: &str, referer: Option<&str>) -> Result<Value, String> {
    let text = get_text(url, referer)?;
    serde_json::from_str(&text).map_err(|error| format!("音源返回的内容无法解析：{error}"))
}

/// 搜索关键词：有艺术家就拼成「标题 艺术家」，提高命中率。
fn search_keyword(title: &str, artist: Option<&str>) -> String {
    match artist {
        Some(artist) => format!("{title} {artist}"),
        None => title.to_string(),
    }
}

/// 从候选里挑最像的一条：标题要能对上，艺术家对上加分。
fn pick_best(candidates: Vec<Candidate>, title: &str, artist: Option<&str>) -> Option<Candidate> {
    candidates
        .into_iter()
        .map(|candidate| {
            let score = score(&candidate, title, artist);
            (score, candidate)
        })
        .filter(|(score, _)| *score > 0)
        .max_by_key(|(score, _)| *score)
        .map(|(_, candidate)| candidate)
}

fn score(candidate: &Candidate, title: &str, artist: Option<&str>) -> i32 {
    let want = normalize(title);
    let got = normalize(&candidate.title);
    let mut score = if got == want {
        100
    } else if !want.is_empty() && (got.contains(&want) || want.contains(&got)) {
        60
    } else {
        return 0;
    };

    if let Some(artist) = artist {
        let want_artist = normalize(artist);
        let got_artist = normalize(&candidate.artist);
        if !want_artist.is_empty()
            && (got_artist.contains(&want_artist) || want_artist.contains(&got_artist))
        {
            score += 40;
        }
    }
    score
}

/// 归一化：丢括号内容（版本 / remix 标注）、空白和常见连接符，全部转小写。
fn normalize(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for character in text.chars() {
        match character {
            '(' | '（' | '[' | '【' => depth += 1,
            ')' | '）' | ']' | '】' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            _ => {
                if !character.is_whitespace() && !matches!(character, '-' | '_' | '·' | '.') {
                    out.extend(character.to_lowercase());
                }
            }
        }
    }
    out
}

/// 查询参数用的百分号编码（只保留 unreserved 字符）。
fn encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// 毫秒时间戳 → 年份。不引日期库，按平均年长估算，够展示用。
fn year_of_millis(millis: i64) -> String {
    const MILLIS_PER_YEAR: i64 = 31_556_952_000;
    if millis <= 0 {
        return String::new();
    }
    (1970 + millis / MILLIS_PER_YEAR).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, title: &str, artist: &str) -> Candidate {
        Candidate {
            id: id.into(),
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            year: String::new(),
            genre: String::new(),
            lyric_url: None,
            cover_url: None,
        }
    }

    #[test]
    fn encodes_chinese_and_spaces() {
        assert_eq!(
            encode("晴天 周杰伦"),
            "%E6%99%B4%E5%A4%A9%20%E5%91%A8%E6%9D%B0%E4%BC%A6"
        );
        assert_eq!(encode("abc-1_2.3~"), "abc-1_2.3~");
    }

    #[test]
    fn normalizes_brackets_and_case() {
        assert_eq!(normalize("Hello（Live版）"), "hello");
        assert_eq!(normalize("晴天 - 周杰伦"), "晴天周杰伦");
        assert_eq!(normalize("【测试】Song"), "song");
    }

    #[test]
    fn picks_exact_title_over_cover_version() {
        let candidates = vec![
            candidate("1", "晴天 (翻唱版)", "RyaVocal"),
            candidate("2", "晴天", "周杰伦"),
        ];
        let best = pick_best(candidates, "晴天", Some("周杰伦")).expect("应该有结果");
        assert_eq!(best.id, "2");
    }

    #[test]
    fn artist_match_breaks_title_ties() {
        let candidates = vec![
            candidate("1", "如果", "别人"),
            candidate("2", "如果", "周杰伦"),
        ];
        let best = pick_best(candidates, "如果", Some("周杰伦")).expect("应该有结果");
        assert_eq!(best.id, "2");
    }

    #[test]
    fn rejects_unrelated_candidates() {
        let candidates = vec![candidate("1", "完全不同的歌", "某人")];
        assert!(pick_best(candidates, "晴天", None).is_none());
    }

    #[test]
    fn parses_netease_search_payload() {
        let payload: Value = serde_json::from_str(
            r#"{"result":{"songs":[{"id":186016,"name":"晴天","artists":[{"name":"周杰伦"}],
            "album":{"name":"叶惠美","publishTime":1057593600000}}]}}"#,
        )
        .expect("样例 JSON 应该合法");
        let candidates = netease_candidates(&payload);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, "186016");
        assert_eq!(candidates[0].artist, "周杰伦");
        assert_eq!(candidates[0].album, "叶惠美");
        assert_eq!(candidates[0].year, "2003");
    }

    #[test]
    fn parses_lx_server_lyric_payload() {
        let payload: Value = serde_json::from_str(
            r#"{"code":0,"msg":"success","data":{"lyric":"[00:00.000]测试","tlyric":""}}"#,
        )
        .expect("样例 JSON 应该合法");
        assert_eq!(parse_lyric(&payload).as_deref(), Some("[00:00.000]测试"));

        // 空歌词 / 纯音乐要返回 None，别把空串写进 .lrc。
        let empty: Value = serde_json::from_str(r#"{"data":{"lyric":"  "}}"#).unwrap();
        assert!(parse_lyric(&empty).is_none());
    }

    #[test]
    fn parses_migu_search_payload() {
        let payload: Value = serde_json::from_str(
            r#"{"songResultData":{"result":[{"id":"3790007","name":"晴天",
            "singers":[{"name":"周杰伦"}],"albums":[{"name":"叶惠美"}],
            "tags":["流行","爱情","原创"],"lyricUrl":"https://example.com/a.lrc",
            "imgItems":[{"img":"https://example.com/a.webp"}]}]}}"#,
        )
        .expect("样例 JSON 应该合法");
        let candidates = migu_candidates(&payload);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].title, "晴天");
        assert_eq!(candidates[0].artist, "周杰伦");
        assert_eq!(candidates[0].album, "叶惠美");
        assert_eq!(candidates[0].genre, "流行");
        assert_eq!(
            candidates[0].lyric_url.as_deref(),
            Some("https://example.com/a.lrc")
        );
    }

    #[test]
    fn writes_lrc_next_to_audio() {
        let dir = std::env::current_dir()
            .expect("取工作目录")
            .join("target")
            .join(format!("lrc-fixture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建测试目录");
        let audio = dir.join("song.mp3");
        std::fs::write(&audio, b"x").expect("写测试音频");

        let written = write_lyric(&audio, "[00:00.000]测试").expect("写歌词");
        assert_eq!(written.file_name().unwrap(), "song.lrc");
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            "[00:00.000]测试"
        );
    }

    /// 联网冒烟测试：`cargo test -- --ignored` 才会跑。
    #[test]
    #[ignore = "需要联网，默认不跑"]
    fn matches_a_real_song() {
        let track = match_track("晴天", Some("周杰伦")).expect("应该能匹配到");
        assert!(track.title.contains("晴天"), "标题不对：{}", track.title);
        assert!(
            track.artist.contains("周杰伦"),
            "艺术家不对：{}",
            track.artist
        );
        assert!(!track.album.is_empty(), "专辑不该为空");
        let lyric = track.lyric.expect("《晴天》应该有歌词");
        assert!(lyric.contains("[00:"), "歌词不是 LRC：{:.40}", lyric);
    }
}
