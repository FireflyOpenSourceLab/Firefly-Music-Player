//! 歌词编辑器窗口。
//!
//! `window` 负责开窗与复用（重复打开时前置已有窗口），`view` 是窗口里的内容，
//! `format` 放 LRC 与 TTML 之间的互转。

use std::path::PathBuf;

pub mod format;
pub mod view;
pub mod window;

/// 编辑器正在为哪首歌工作。
///
/// 托盘菜单打开的是「空白编辑器」，没有绑定歌曲；从音乐库右键「修改歌词」进来时
/// 带着这首歌，保存时才知道歌词该写回哪个文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Song {
    /// 音频文件路径；歌词写到同目录同名的 `.lrc` 与文件内嵌歌词标签。
    pub path: PathBuf,
    /// 展示用标题。
    pub title: String,
    /// 艺术家，标签和文件名里都没有时为空。
    pub artist: Option<String>,
}

impl Song {
    /// 提示框里显示的歌名，例如 `《晴天》—— 周杰伦`。
    pub fn display(&self) -> String {
        match self
            .artist
            .as_deref()
            .map(str::trim)
            .filter(|artist| !artist.is_empty())
        {
            Some(artist) => format!("《{}》—— {artist}", self.title),
            None => format!("《{}》", self.title),
        }
    }

    /// 歌词文件路径：与音频同名的 `.lrc`。
    pub fn lyric_path(&self) -> PathBuf {
        self.path.with_extension("lrc")
    }
}
