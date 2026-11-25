//! 我收藏的。

use super::placeholder;
use gpui_icons::LucideIcon;
use gpui_kit::{AnyElement, App};

/// 渲染「我收藏的」页。
pub fn view(cx: &mut App) -> AnyElement {
    placeholder(
        LucideIcon::Bookmark,
        "我收藏的",
        "你收藏的专辑与歌单会出现在这里",
        cx,
    )
}
