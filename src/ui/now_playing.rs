//! 「正在播放」面板的数据模型：播放列表、进度、循环/随机状态与歌词。
//!
//! 这里只放纯数据与派生逻辑（不依赖 UI），面板视图在 `main.rs` 里绘制，
//! 这样后续接入真实音频引擎时，替换的是播放源而不是界面。

/// 一首曲目的展示信息。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Track {
    pub title: &'static str,
    pub artist: &'static str,
    pub album: &'static str,
    /// 时长（秒）。
    pub duration: f32,
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
pub type LyricLine = (f32, &'static str);

/// 演示用的播放列表。
pub const TRACKS: [Track; 3] = [
    Track {
        title: "夜航",
        artist: "Firefly Studio",
        album: "夜色电台",
        duration: 214.,
    },
    Track {
        title: "旧磁带",
        artist: "Firefly Studio",
        album: "回声巷",
        duration: 186.,
    },
    Track {
        title: "清晨六点",
        artist: "Firefly Studio",
        album: "回声巷",
        duration: 198.,
    },
];

/// 每首曲目的歌词，与 [`TRACKS`] 按下标一一对应。
pub const LYRICS: [&[LyricLine]; TRACKS.len()] = [
    &[
        (0., "夜色落在窗台"),
        (14., "风把旋律吹进来"),
        (28., "城市安静得像一片海"),
        (42., "我沿着灯火慢慢航行"),
        (56., "别问终点在哪里"),
        (70., "让心跳当作罗盘"),
        (84., "每一次呼吸都是节拍"),
        (98., "萤火亮起 就是方向"),
    ],
    &[
        (0., "抽屉里的旧磁带"),
        (12., "还留着夏天的温度"),
        (26., "指针转了一圈又一圈"),
        (40., "把故事都唱回从前"),
        (54., "如果时间可以倒带"),
        (68., "我想停在那一年"),
    ],
    &[
        (0., "清晨六点的光"),
        (13., "落在没喝完的咖啡上"),
        (27., "窗外的树影慢慢移动"),
        (41., "新的一天开始生长"),
        (55., "把昨天轻轻放下"),
        (69., "让风带走所有喧哗"),
    ],
];

/// 播放器的当前状态。
#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    /// 当前曲目在 [`TRACKS`] 中的下标。
    pub index: usize,
    /// 播放进度，`0..=1`。用归一化进度而不是秒数，切换曲目时进度条无需重建。
    pub progress: f32,
    /// 是否正在播放。
    pub playing: bool,
    /// 是否随机播放（演示用，仅切换按钮状态）。
    pub shuffle: bool,
    /// 循环模式。
    pub repeat: RepeatMode,
}

impl Default for NowPlaying {
    fn default() -> Self {
        Self {
            index: 0,
            progress: 0.,
            playing: false,
            shuffle: false,
            repeat: RepeatMode::default(),
        }
    }
}

impl NowPlaying {
    /// 当前曲目。
    pub fn track(&self) -> &'static Track {
        &TRACKS[self.index]
    }

    /// 当前曲目的歌词。
    pub fn lyrics(&self) -> &'static [LyricLine] {
        LYRICS[self.index]
    }

    /// 当前播放位置（秒）。
    pub fn position(&self) -> f32 {
        self.progress * self.track().duration
    }

    /// 跳到归一化进度，自动夹在 `0..=1`。
    pub fn seek(&mut self, progress: f32) {
        self.progress = progress.clamp(0., 1.);
    }

    /// 切换播放 / 暂停。
    pub fn toggle_play(&mut self) {
        self.playing = !self.playing;
    }

    /// 切换随机播放。
    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
    }

    /// 切换循环模式。
    pub fn cycle_repeat(&mut self) {
        self.repeat = self.repeat.cycled();
    }

    /// 下一首，到末尾回绕到第一首。
    pub fn next_track(&mut self) {
        self.index = (self.index + 1) % TRACKS.len();
        self.progress = 0.;
    }

    /// 上一首：播放超过 3 秒时先回到本曲开头，否则切到上一首（播放器通行行为）。
    pub fn prev_track(&mut self) {
        if self.position() > 3. {
            self.progress = 0.;
            return;
        }
        self.index = if self.index == 0 {
            TRACKS.len() - 1
        } else {
            self.index - 1
        };
        self.progress = 0.;
    }

    /// 前进 `dt` 秒，并在播到末尾时按循环模式处理。
    pub fn advance(&mut self, dt: f32) {
        let duration = self.track().duration;
        if duration <= 0. {
            return;
        }
        self.progress += dt / duration;
        if self.progress >= 1. {
            self.progress = 1.;
            self.handle_track_end();
        }
    }

    /// 一曲结束后的去向。
    fn handle_track_end(&mut self) {
        match self.repeat {
            RepeatMode::One => self.progress = 0.,
            RepeatMode::All => {
                self.index = (self.index + 1) % TRACKS.len();
                self.progress = 0.;
            }
            RepeatMode::Off => {
                if self.index + 1 < TRACKS.len() {
                    self.index += 1;
                    self.progress = 0.;
                } else {
                    // 列表放完：停在最后一曲末尾。
                    self.playing = false;
                }
            }
        }
    }

    /// 当前应高亮的歌词行；播放位置在首行之前时返回第 0 行。
    pub fn current_line(&self) -> usize {
        let position = self.position();
        self.lyrics()
            .iter()
            .rposition(|(start, _)| *start <= position)
            .unwrap_or(0)
    }

    /// 已播放时长，如 `1:23`。
    pub fn elapsed_label(&self) -> String {
        format_time(self.position())
    }

    /// 曲目总时长，如 `3:34`。
    pub fn duration_label(&self) -> String {
        format_time(self.track().duration)
    }
}

/// 秒数格式化成 `m:ss`。
pub fn format_time(secs: f32) -> String {
    let total = secs.max(0.).round() as u32;
    format!("{}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_format_pads_seconds() {
        assert_eq!(format_time(0.), "0:00");
        assert_eq!(format_time(61.4), "1:01");
        assert_eq!(format_time(214.), "3:34");
    }

    #[test]
    fn progress_maps_to_position() {
        let mut now = NowPlaying::default();
        now.seek(0.5);
        assert_eq!(now.position(), TRACKS[0].duration / 2.);
        now.seek(2.);
        assert_eq!(now.progress, 1.);
    }

    #[test]
    fn lyric_line_follows_position() {
        let mut now = NowPlaying::default();
        assert_eq!(now.current_line(), 0);
        now.seek(60. / TRACKS[0].duration); // 60 秒处落在第 5 行（56s）
        assert_eq!(now.current_line(), 4);
    }

    #[test]
    fn repeat_cycles_off_all_one() {
        let mut now = NowPlaying::default();
        assert_eq!(now.repeat, RepeatMode::Off);
        now.cycle_repeat();
        assert_eq!(now.repeat, RepeatMode::All);
        now.cycle_repeat();
        assert_eq!(now.repeat, RepeatMode::One);
        now.cycle_repeat();
        assert_eq!(now.repeat, RepeatMode::Off);
    }

    #[test]
    fn end_of_list_stops_when_not_repeating() {
        let mut now = NowPlaying::default();
        now.index = TRACKS.len() - 1;
        now.seek(1.);
        now.advance(0.1);
        assert!(!now.playing);
        assert_eq!(now.index, TRACKS.len() - 1);
    }

    #[test]
    fn end_of_track_advances_when_repeating_all() {
        let mut now = NowPlaying::default();
        now.repeat = RepeatMode::All;
        now.seek(1.);
        now.advance(0.1);
        assert_eq!(now.index, 1);
        assert_eq!(now.progress, 0.);
    }
}
