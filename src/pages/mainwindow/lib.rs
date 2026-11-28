//! 主窗口的页面集合。
//!
//! 每个模块对应侧边栏的一个导航项（设置页尚未实现）：
//!
//! | 模块         | 侧边栏        |
//! | ------------ | ------------- |
//! | `home`       | 主页          |
//! | `library`    | 音乐库        |
//! | `artist`     | 艺术家        |
//! | `zhuanji`    | 专辑          |
//! | `myfavorite` | 我喜欢的      |
//! | `mystars`    | 我收藏的      |
//!
//! 各页面统一暴露 `pub fn view(cx: &mut App) -> AnyElement`，
//! 由 `main.rs` 里 `Destination::page` 负责分发。

pub mod artist;
pub mod home;
pub mod library;
pub mod metadata;
pub mod myfavorite;
pub mod mystars;
pub mod zhuanji;

use crate::ui::theme::UiTheme;
use gpui_icons::LucideIcon;
use gpui_kit::{AnyElement, App, FontWeight, IntoElement, ParentElement as _, Styled, div, px};

/// 还没有真实内容的页面骨架：圆形图标 + 标题 + 一句说明，居中显示。
///
/// 页面接入真实内容时直接替换各自 `view` 的函数体即可，不必再调用它。
pub fn placeholder(
    icon: LucideIcon,
    title: &'static str,
    hint: &'static str,
    cx: &mut App,
) -> AnyElement {
    let theme = UiTheme::read(cx).clone();
    let s = theme.spacing.unit;

    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(s * 2.5_f32)
        .text_color(theme.colors.foreground)
        .font_family(theme.fonts.body.clone())
        .child(
            div()
                .size(px(56.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.colors.muted)
                .child(
                    gpui_icons::lucide(icon)
                        .size(px(24.))
                        .text_color(theme.colors.muted_foreground),
                ),
        )
        .child(
            div()
                .text_size(px(26.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_size(theme.text(13.))
                .text_color(theme.colors.muted_foreground)
                .child(hint),
        )
        .into_any_element()
}
