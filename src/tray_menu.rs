//! 系统托盘：原生托盘图标 + 菜单。
//!
//! 这个模块只负责「菜单长什么样」和「点了派发哪个 action」；具体行为由
//! `main.rs` 里注册的动作处理函数实现，所以托盘不直接改播放器状态。
//!
//! 菜单文案取自 [`TrayModel`]。原生菜单不会自己跟着 Rust 状态变，因此每次
//! 状态变化后都要调用一次 [`refresh`]，让它按最新的 `TrayModel` 重建。

use std::rc::Rc;

use gpui_kit::{App, Global, MenuItem, WeakEntity, actions};
use gpui_tray::{Icon, Tray};

use crate::Player;

actions!(
    firefly_tray,
    [
        /// 曲目信息那一行的占位动作；该行始终禁用，不会真的派发。
        TrayNoop,
        /// 下一曲。
        TrayNext,
        /// 上一曲。
        TrayPrevious,
        /// 播放 / 暂停。
        TrayTogglePlay,
        /// 打开（前置）主面板。
        TrayOpenWindow,
        /// 打开歌词编辑器窗口。
        TrayOpenLyricsEditor,
        /// 循环播放开关。
        TrayToggleLoop,
        /// 退出应用。
        TrayQuit,
    ]
);

/// 没有可播放媒体时，菜单首行显示的占位文案。
const MEDIA_PLACEHOLDER: &str = "无媒体";

/// 托盘菜单需要的状态快照。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayModel {
    /// 已格式化的「曲名 - 艺术家」；`None` 表示当前没有媒体。
    pub track: Option<String>,
    /// 是否正在播放，决定第二组里显示「暂停」还是「播放」。
    pub playing: bool,
    /// 循环播放是否开启，对应菜单里的勾选项。
    pub looping: bool,
}

impl TrayModel {
    /// 菜单首行的文案。
    pub fn label(&self) -> String {
        self.track
            .clone()
            .unwrap_or_else(|| MEDIA_PLACEHOLDER.to_string())
    }
}

/// 按状态拼出菜单项。
fn build_menu(model: &TrayModel) -> Vec<MenuItem> {
    vec![
        // 曲目信息只做展示，所以禁用掉，点它不会有任何反应。
        MenuItem::action(model.label(), TrayNoop).disabled(true),
        MenuItem::separator(),
        MenuItem::action("下一曲", TrayNext),
        MenuItem::action(if model.playing { "暂停" } else { "播放" }, TrayTogglePlay),
        MenuItem::action("上一曲", TrayPrevious),
        MenuItem::action("打开主面板", TrayOpenWindow),
        MenuItem::action("歌词编辑器", TrayOpenLyricsEditor),
        MenuItem::action("循环播放", TrayToggleLoop).checked(model.looping),
        MenuItem::separator(),
        MenuItem::action("退出", TrayQuit),
    ]
}

/// 托盘句柄与状态读取函数。
///
/// `Tray` 本身必须活到应用退出，挂成全局对象最省事；顺带把 `model` 存下来，
/// 这样菜单构建闭包和外部刷新用的是同一份状态来源。
struct TrayHandle {
    tray: Tray,
    model: Rc<dyn Fn(&App) -> TrayModel>,
}

impl Global for TrayHandle {}

/// 创建托盘。`player` 用来读取菜单要展示的状态。
pub fn install(cx: &mut App, player: WeakEntity<Player>) -> gpui_tray::Result<()> {
    let model: Rc<dyn Fn(&App) -> TrayModel> = Rc::new(move |cx: &App| {
        player
            .upgrade()
            .map(|player| player.read(cx).tray_model(cx))
            .unwrap_or_default()
    });

    let menu_model = model.clone();
    let tray = Tray::builder()
        .icon(icon()?)
        .title("Firefly Music")
        .tooltip("Firefly Music")
        .menu(move |cx| build_menu(&menu_model(cx)))
        .build(cx)?;

    cx.set_global(TrayHandle { tray, model });
    Ok(())
}

/// 按最新的播放状态重建原生菜单。
///
/// 托盘没装上（例如 Linux 没有 StatusNotifierItem 宿主）时静默跳过。
pub fn refresh(cx: &mut App) {
    if !cx.has_global::<TrayHandle>() {
        return;
    }
    let tray = cx.global::<TrayHandle>().tray.clone();
    if let Err(error) = tray.refresh_menu(cx) {
        eprintln!("刷新托盘菜单失败: {error}");
    }
}

/// 移除托盘图标。托盘没装上时什么也不做。
pub fn close(cx: &mut App) {
    if !cx.has_global::<TrayHandle>() {
        return;
    }
    let tray = cx.global::<TrayHandle>().tray.clone();
    let _ = tray.close(cx);
}

/// 托盘图标：构建时嵌进来的裸 RGBA 像素，省掉运行时解码 PNG。
///
/// macOS 菜单栏高 18pt，用 36px 图并声明 2 倍缩放；Windows / Linux 直接用 32px。
#[cfg(target_os = "macos")]
fn icon() -> gpui_tray::Result<Icon> {
    const SIZE: u32 = 36;
    const RGBA: &[u8] = include_bytes!("../assets/icon/tray-36.rgba");
    Icon::from_rgba(RGBA, SIZE, SIZE).scale_factor(2.0).build()
}

/// 托盘图标：构建时嵌进来的裸 RGBA 像素，省掉运行时解码 PNG。
#[cfg(not(target_os = "macos"))]
fn icon() -> gpui_tray::Result<Icon> {
    const SIZE: u32 = 32;
    const RGBA: &[u8] = include_bytes!("../assets/icon/tray-32.rgba");
    Icon::from_rgba(RGBA, SIZE, SIZE).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_when_no_media() {
        let model = TrayModel::default();
        assert_eq!(model.label(), "无媒体");
    }

    #[test]
    fn label_uses_track_when_present() {
        let model = TrayModel {
            track: Some("夜航 - Firefly Studio".into()),
            ..TrayModel::default()
        };
        assert_eq!(model.label(), "夜航 - Firefly Studio");
    }

    /// 菜单顺序：曲目 / 分隔 / 下一曲 / 播放暂停 / 上一曲 / 打开主面板 /
    /// 歌词编辑器 / 循环 / 分隔 / 退出。
    #[test]
    fn menu_order_and_state() {
        let model = TrayModel {
            track: None,
            playing: true,
            looping: true,
        };
        let items = build_menu(&model);
        assert_eq!(items.len(), 10);

        // 曲目行禁用，文案为占位
        assert!(matches!(
            &items[0],
            MenuItem::Action { name, disabled, .. }
                if name.as_ref() == "无媒体" && *disabled
        ));
        assert!(matches!(items[1], MenuItem::Separator));
        assert!(matches!(
            &items[2],
            MenuItem::Action { name, .. } if name.as_ref() == "下一曲"
        ));
        // 正在播放时显示「暂停」
        assert!(matches!(
            &items[3],
            MenuItem::Action { name, .. } if name.as_ref() == "暂停"
        ));
        assert!(matches!(
            &items[4],
            MenuItem::Action { name, .. } if name.as_ref() == "上一曲"
        ));
        assert!(matches!(
            &items[5],
            MenuItem::Action { name, .. } if name.as_ref() == "打开主面板"
        ));
        assert!(matches!(
            &items[6],
            MenuItem::Action { name, .. } if name.as_ref() == "歌词编辑器"
        ));
        // 勾选项
        assert!(items[7].is_checked());
        assert!(matches!(items[8], MenuItem::Separator));
        assert!(matches!(
            &items[9],
            MenuItem::Action { name, .. } if name.as_ref() == "退出"
        ));
    }

    #[test]
    fn paused_label() {
        let model = TrayModel::default();
        let items = build_menu(&model);
        assert!(matches!(
            &items[3],
            MenuItem::Action { name, .. } if name.as_ref() == "播放"
        ));
        assert!(!items[7].is_checked());
    }
}
