//! 我喜欢的。

use super::placeholder;
use gpui_icons::LucideIcon;
use gpui_kit::{AnyElement, App};

/// 渲染「我喜欢的」页。
pub fn view(cx: &mut App) -> AnyElement {
    placeholder(
        LucideIcon::Heart,
        "我喜欢的",
        "你点过喜欢的歌曲会出现在这里",
        cx,
    )
}
