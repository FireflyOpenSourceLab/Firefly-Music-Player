//! 音乐库。

use super::placeholder;
use gpui_icons::LucideIcon;
use gpui_kit::{AnyElement, App};

/// 渲染音乐库页。
pub fn view(cx: &mut App) -> AnyElement {
    placeholder(
        LucideIcon::Library,
        "音乐库",
        "本地歌曲、文件夹与扫描结果会出现在这里",
        cx,
    )
}
