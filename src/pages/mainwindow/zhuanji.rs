//! 专辑。

use super::placeholder;
use gpui_icons::LucideIcon;
use gpui_kit::{AnyElement, App};

/// 渲染专辑页。
pub fn view(cx: &mut App) -> AnyElement {
    placeholder(
        LucideIcon::Disc3,
        "专辑",
        "按专辑浏览你的音乐",
        cx,
    )
}
