//! 主页。

use super::placeholder;
use gpui_icons::LucideIcon;
use gpui_kit::{AnyElement, App};

/// 渲染主页。
pub fn view(cx: &mut App) -> AnyElement {
    placeholder(
        LucideIcon::House,
        "主页",
        "最近播放、推荐歌单会出现在这里",
        cx,
    )
}
