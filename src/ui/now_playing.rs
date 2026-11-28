//! 「正在播放」的数据模型：队列、播放位置、循环模式与侧车歌词。
//!
//! 这里只放纯数据与派生逻辑，不碰音频设备；真正的解码与输出在 `crate::audio`。
//! 界面每帧读一份快照来画浮动栏和进度，所以这个结构要能廉价地 clone。

use std::path::{Path, PathBuf};

/// 队列里的一首歌。标题 / 艺术家都来自文件名与标签解析，允许为空串。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
}

impl Track {
    pub fn new(path: impl Into<PathBuf>, title: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            title: title.into(),
            artist: String::new(),
            album: String::new(),
        }
    }

    /// 补上艺术家。
    pub fn with_artist(mut self, artist: impl Into<String>) -> Self {
        self.artist = artist.into();
        self
    }

    /// 补上专辑。
    pub fn with_album(mut self, album: impl Into<String>) -> Self {
        self.album = album.into();
        self
    }

    /// 副标题：`艺术家 · 专辑`，缺哪个省哪个。
    pub fn subtitle(&self) -> String {
        match (self.artist.trim(), self.album.trim()) {
            ("", "") => String::new(),
            (artist, "") => artist.to_string(),
            ("", album) => album.to_string(),
            (artist, album) => format!("{artist} · {album}"),
        }
    }
}

/// 循环模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepeatMode {
    /// 不循环：播放到列表末尾停止。
    #[default]
    Off,
    /// 列表循环。
    All,
    /// 单曲循环。
    One,
}

impl RepeatMode {
    /// 依次切换：关 → 列表循环 → 单曲循环 → 关。
    pub fn cycled(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    /// 是否处于开启状态，用于按钮高亮。
    pub fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// 一行歌词：起始秒数与文本。
#[derive(Clone, Debug, PartialEq)]
pub struct LyricLine {
    pub at: f32,
    pub text: String,
}

/// 播放器状态快照。
#[derive(Clone, Debug, Default)]
pub struct NowPlaying {
    /// 播放队列（点某首歌播放时，整个文件夹都进队列）。
    pub queue: Vec<Track>,
    /// 当前曲目在队列里的下标。
    pub index: usize,
    /// 是否正在出声。
    pub playing: bool,
    /// 循环模式。
    pub repeat: RepeatMode,
    /// 播放位置（秒）。
    pub position: f32,
    /// 当前曲目总时长（秒）；解码器与容器头都给不出时是 0（界面显示 `--:--`）。
    pub duration: f32,
    /// 侧车 `.lrc` 解析出的歌词，按时间升序。
    pub lyrics: Vec<LyricLine>,
    /// 上一次播放失败的原因；成功后清空。
    pub error: Option<String>,
}

impl NowPlaying {
    /// 当前曲目。
    pub fn current(&self) -> Option<&Track> {
        self.queue.get(self.index)
    }

    /// 归一化进度，`0..=1`；时长未知时返回 0。
    pub fn progress(&self) -> f32 {
        if self.duration <= 0. {
            0.
        } else {
            (self.position / self.duration).clamp(0., 1.)
        }
    }

    /// 把归一化进度换算成秒并落回位置。
    pub fn seek(&mut self, progress: f32) {
        if self.duration > 0. {
            self.position = progress.clamp(0., 1.) * self.duration;
        }
    }

    /// 列表里是否还有歌。
    pub fn has_queue(&self) -> bool {
        !self.queue.is_empty()
    }

    /// 下一首的下标；循环按调用方决定是否回绕，这里只做「末尾不动」。
    pub fn next_index(&self) -> Option<usize> {
        (self.index + 1 < self.queue.len()).then_some(self.index + 1)
    }

    /// 上一首的下标；开头再往前返回 `None`。
    pub fn prev_index(&self) -> Option<usize> {
        self.index.checked_sub(1)
    }

    /// 当前应高亮的歌词行下标；没有歌词或还没到第一行时返回 0。
    pub fn current_line(&self) -> usize {
        self.lyrics
            .iter()
            .rposition(|line| line.at <= self.position)
            .unwrap_or(0)
    }

    /// 浮动栏上显示的那一句歌词：优先用歌词，没有歌词就退回曲名。
    pub fn current_lyric(&self) -> String {
        match (self.lyrics.is_empty(), self.current()) {
            (false, _) => self.lyrics[self.current_line()].text.clone(),
            (true, Some(track)) => track.title.clone(),
            (true, None) => "还没有在播放".to_string(),
        }
    }

    /// 已播放时长，如 `1:23`。
    pub fn elapsed_label(&self) -> String {
        format_time(self.position)
    }

    /// 曲目总时长，如 `3:34`；未知时是 `--:--`。
    pub fn duration_label(&self) -> String {
        if self.duration > 0. {
            format_time(self.duration)
        } else {
            "--:--".to_string()
        }
    }
}

/// 秒数格式化成 `m:ss`。
pub fn format_time(secs: f32) -> String {
    let total = secs.max(0.).round() as u32;
    format!("{}:{:02}", total / 60, total % 60)
}

/// 找音频旁边的同名 `.lrc` 并解析；没有就返回空。
pub fn load_lyrics(audio: &Path) -> Vec<LyricLine> {
    let Some(stem) = audio.file_stem() else {
        return Vec::new();
    };
    let mut lrc = audio.with_file_name(stem);
    lrc.set_extension("lrc");
    match std::fs::read_to_string(&lrc) {
        Ok(text) => parse_lrc(&text),
        Err(_) => Vec::new(),
    }
}

/// 解析 LRC：支持一行多个时间标签，毫秒位 2~3 位可有可无。
///
/// 除了 `[mm:ss.xx]` 时间标签，其余方括号标签（`[ar:]`、`[ti:]` 等）一律跳过。
pub fn parse_lrc(text: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim_start();
        let mut stamps = Vec::new();
        // 行首可以连着一串时间标签。
        while let Some(after) = rest.strip_prefix('[') {
            let Some(close) = after.find(']') else { break };
            let tag = &after[..close];
            rest = &after[close + 1..];
            if let Some(seconds) = parse_time_tag(tag) {
                stamps.push(seconds);
            }
        }
        let content = rest.trim();
        if content.is_empty() {
            continue;
        }
        for at in stamps {
            lines.push(LyricLine {
                at,
                text: content.to_string(),
            });
        }
    }
    lines.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(std::cmp::Ordering::Equal));
    lines
}

/// `02:31.42` / `02:31` → 秒。不是时间格式返回 `None`。
fn parse_time_tag(tag: &str) -> Option<f32> {
    let (minutes, rest) = tag.split_once(':')?;
    let minutes: f32 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = match rest.split_once(['.', ':']) {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (rest, None),
    };
    let seconds: f32 = seconds.trim().parse().ok()?;
    let fraction = fraction
        .map(|fraction| {
            let digits: String = fraction.chars().take(3).collect();
            let value: f32 = digits.parse().unwrap_or(0.);
            // 两位是百分秒，三位是毫秒。
            value / 10f32.powi(digits.len() as i32)
        })
        .unwrap_or(0.);
    Some(minutes * 60. + seconds + fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> Vec<Track> {
        vec![
            Track::new("/music/a.mp3", "A"),
            Track::new("/music/b.mp3", "B"),
            Track::new("/music/c.mp3", "C"),
        ]
    }

    #[test]
    fn time_format_pads_seconds() {
        assert_eq!(format_time(0.), "0:00");
        assert_eq!(format_time(61.4), "1:01");
        assert_eq!(format_time(214.), "3:34");
    }

    #[test]
    fn progress_is_clamped_and_zero_when_duration_unknown() {
        let mut now = NowPlaying::default();
        now.duration = 200.;
        now.seek(0.5);
        assert_eq!(now.position, 100.);
        now.seek(2.);
        assert_eq!(now.position, 200.);

        let mut unknown = NowPlaying::default();
        unknown.seek(0.5);
        assert_eq!(unknown.position, 0.);
        assert_eq!(unknown.duration_label(), "--:--");
    }

    #[test]
    fn repeat_cycles_off_all_one() {
        let mut mode = RepeatMode::default();
        assert_eq!(mode, RepeatMode::Off);
        mode = mode.cycled();
        assert_eq!(mode, RepeatMode::All);
        mode = mode.cycled();
        assert_eq!(mode, RepeatMode::One);
        assert_eq!(mode.cycled(), RepeatMode::Off);
    }

    #[test]
    fn lyric_line_follows_position() {
        let mut now = NowPlaying::default();
        now.lyrics = parse_lrc("[00:00.00]第一行\n[00:30.00]第二行\n[01:00.00]第三行");
        assert_eq!(now.current_line(), 0);
        now.position = 45.;
        assert_eq!(now.current_line(), 1);
        now.position = 999.;
        assert_eq!(now.current_line(), 2);
        assert_eq!(now.current_lyric(), "第三行");
    }

    #[test]
    fn current_lyric_falls_back_to_title() {
        let mut now = NowPlaying::default();
        now.queue = queue();
        assert_eq!(now.current_lyric(), "A");
        assert!(NowPlaying::default().current_lyric().contains("还没有"));
    }

    #[test]
    fn queue_navigation_stops_at_edges() {
        let mut now = NowPlaying::default();
        now.queue = queue();
        assert_eq!(now.next_index(), Some(1));
        assert_eq!(now.prev_index(), None);
        now.index = 2;
        assert_eq!(now.next_index(), None);
        assert_eq!(now.prev_index(), Some(1));
    }

    #[test]
    fn parses_multiple_time_tags_on_one_line() {
        let lines = parse_lrc("[00:01.00][00:05.50]副歌\n[ar:某人]\n[00:09]尾奏");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].at, 1.);
        assert_eq!(lines[1].at, 5.5);
        assert_eq!(lines[1].text, "副歌");
        assert_eq!(lines[2].at, 9.);
        assert_eq!(lines[2].text, "尾奏");
    }

    #[test]
    fn parses_three_digit_milliseconds() {
        let lines = parse_lrc("[01:02.345]一行");
        assert_eq!(lines[0].at, 62.345);
    }

    #[test]
    fn subtitle_skips_missing_parts() {
        assert_eq!(Track::new("/a.mp3", "A").subtitle(), "");
        assert_eq!(
            Track::new("/a.mp3", "A").with_artist("歌手").subtitle(),
            "歌手"
        );
        assert_eq!(
            Track::new("/a.mp3", "A")
                .with_artist("歌手")
                .with_album("专辑")
                .subtitle(),
            "歌手 · 专辑"
        );
    }
}
