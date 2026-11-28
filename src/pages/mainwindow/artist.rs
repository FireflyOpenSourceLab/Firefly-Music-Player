//! 艺术家。

use super::placeholder;
use gpui_icons::LucideIcon;
use gpui_kit::{AnyElement, App};

/// 渲染艺术家页。
pub fn view(cx: &mut App) -> AnyElement {
    placeholder(LucideIcon::UserRound, "艺术家", "按艺术家浏览你的音乐", cx)
}
