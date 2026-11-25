#![allow(dead_code)]

mod pages;
mod tray_menu;
mod ui;

use gpui_icons::LucideIcon;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled, Window, WindowDecorations, WindowOptions, div, point,
    prelude::FluentBuilder as _, px, size,
};
use gpui_kit::component::slider::{SliderEvent, SliderState};
use std::time::Duration;
use ui::{
    app::{AppConfig, run},
    app_theme,
    input::{Input, InputEvent, InputState},
    now_playing::NowPlaying,
    sidebar::{
        Sidebar, SidebarCollapsible, SidebarItem, SidebarLayout, SidebarState, sidebar_menu,
        sidebar_separator,
    },
    theme::UiTheme,
};

/// 自绘标题栏高度；macOS 红绿灯在其中的垂直位置据此居中。
const TITLEBAR_HEIGHT: f32 = 40.;

/// 主窗口最小尺寸（逻辑像素）。
const MIN_WINDOW_WIDTH: f32 = 900.;
const MIN_WINDOW_HEIGHT: f32 = 600.;

/// 内容区底部浮动栏：高度、圆角，以及离左右两端与底部的间距。
const FLOAT_BAR_HEIGHT: f32 = 70.;
const FLOAT_BAR_RADIUS: f32 = 35.;
const FLOAT_BAR_INSET_X: f32 = 120.;
const FLOAT_BAR_INSET_BOTTOM: f32 = 12.;

/// 页面显示区左上角的圆角半径。
const PAGE_CORNER_RADIUS: f32 = 20.;

/// 窗口宽度小于等于该值时，侧边栏自动收缩成图标栏（不覆盖用户的展开偏好）。
const SIDEBAR_COLLAPSE_WIDTH: f32 = 950.;

/// 窗口是否已窄到需要把侧边栏收缩成图标栏。
fn sidebar_narrow(width: gpui_kit::Pixels) -> bool {
    width <= px(SIDEBAR_COLLAPSE_WIDTH)
}

/// 播放计时的节拍：毫秒与对应的秒数。
const TICK_MS: u64 = 250;
const TICK_SECS: f32 = TICK_MS as f32 / 1000.;

fn main() {
    // Opens the window with the Lucide icons, the HarmonyOS Sans fonts and the
    // theme pair from assets/theme.css; `gpuicn theme set` changes the base
    // color, radius and appearance.
    run(
        AppConfig::new("Firefly Music (Player)")
            .themes(app_theme::light(), app_theme::dark())
            .appearance(app_theme::APPEARANCE)
            .window_options(WindowOptions {
                // macOS / Windows：隐藏系统标题栏，改用自绘标题栏。
                // Linux（X11/Wayland）：用客户端装饰隐藏服务端标题栏。
                titlebar: Some(gpui_kit::TitlebarOptions {
                    title: None, // 由 AppConfig::install 填入窗口标题
                    appears_transparent: true,
                    // macOS 系统红绿灯左上角坐标，垂直居中于自绘标题栏。
                    traffic_light_position: Some(point(px(16.), px((TITLEBAR_HEIGHT - 14.) / 2.))),
                }),
                window_decorations: Some(WindowDecorations::Client),
                // 主窗口最小尺寸 900×600。
                window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
                // macOS：自绘标题栏时由应用自己处理拖动（start_window_move）。
                app_owns_titlebar_drag: true,
                ..Default::default()
            }),
        |window, cx| Player::new(window, cx),
    );
}

/// 侧边栏的导航目的地。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Destination {
    Home,
    Library,
    Artists,
    Albums,
    Liked,
    Favorites,
    Settings,
}

impl Destination {
    /// 全部目的地，按侧边栏顺序排列。
    const ALL: [Self; 7] = [
        Self::Home,
        Self::Library,
        Self::Artists,
        Self::Albums,
        Self::Liked,
        Self::Favorites,
        Self::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Home => "主页",
            Self::Library => "音乐库",
            Self::Artists => "艺术家",
            Self::Albums => "专辑",
            Self::Liked => "我喜欢的",
            Self::Favorites => "我收藏的",
            Self::Settings => "设置",
        }
    }

    fn icon(self) -> LucideIcon {
        match self {
            Self::Home => LucideIcon::House,
            Self::Library => LucideIcon::Library,
            Self::Artists => LucideIcon::UserRound,
            Self::Albums => LucideIcon::Disc3,
            Self::Liked => LucideIcon::Heart,
            Self::Favorites => LucideIcon::Bookmark,
            Self::Settings => LucideIcon::Settings,
        }
    }

    /// 稳定的英文键，用于元素 id 与英文关键词匹配。
    fn key(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Library => "library",
            Self::Artists => "artists",
            Self::Albums => "albums",
            Self::Liked => "liked",
            Self::Favorites => "favorites",
            Self::Settings => "settings",
        }
    }

    /// 该导航项对应的页面内容。
    ///
    /// 六个内容页放在 `pages::mainwindow` 下，各自的 `view` 返回页面元素；
    /// 设置页还没做，先用同一套占位骨架。
    fn page(self, cx: &mut App) -> AnyElement {
        use pages::mainwindow as mw;

        match self {
            Self::Home => mw::home::view(cx),
            Self::Library => mw::library::view(cx),
            Self::Artists => mw::artist::view(cx),
            Self::Albums => mw::zhuanji::view(cx),
            Self::Liked => mw::myfavorite::view(cx),
            Self::Favorites => mw::mystars::view(cx),
            Self::Settings => {
                mw::placeholder(LucideIcon::Settings, "设置", "设置页还在建设中", cx)
            }
        }
    }
}

struct Player {
    /// 当前选中的导航项。
    active: Destination,
    /// 侧边栏外壳状态（桌面展开 / 移动端抽屉）。
    sidebar: SidebarState,
    /// spotlight 搜索的输入状态。
    search: Entity<InputState>,
    /// spotlight 是否打开。
    search_open: bool,
    /// 右侧「正在播放」面板的状态。
    now: NowPlaying,
    /// 进度条状态：值域归一化为 0..=1，拖动的结果写回 `now.progress`。
    seek: Entity<SliderState>,
    /// 播放计时器的代次；每次开始/停止播放都 +1，旧任务据此自行退出。
    tick_generation: u64,
}

impl Player {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("在这里搜索歌曲、歌手、专辑信息")
        });
        // 进度条用归一化值域（0..=1），切歌时无需重建状态。
        let seek = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.001));
        // 拖动 / 点击进度条时把归一化进度写回播放状态。
        cx.subscribe(&seek, |this, _, event: &SliderEvent, cx| {
            if let SliderEvent::Change(value) = event {
                this.now.seek(value.end());
                cx.notify();
            }
        })
        .detach();
        // 输入变化时重绘结果；回车选中第一条结果并关闭。
        cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) && this.search_open {
                if let Some(first) = this.filtered(&*cx).into_iter().next() {
                    this.active = first;
                }
                this.search_open = false;
            }
            cx.notify();
        })
        .detach();

        let player = Self {
            active: Destination::Home,
            sidebar: SidebarState::default(),
            search,
            search_open: false,
            now: NowPlaying::default(),
            seek,
            tick_generation: 0,
        };
        // 托盘要在实体存在之后装：菜单动作要回写这个实体。
        player.install_tray(cx);
        player
    }

    /// 安装系统托盘，并把菜单动作接到播放器上。
    ///
    /// 托盘建不起来（比如 Linux 没有 StatusNotifierItem 宿主）不该拖垮应用，
    /// 所以失败只打日志、界面照常显示。
    fn install_tray(&self, cx: &mut Context<Self>) {
        let player = cx.weak_entity();
        // `Context` 自己的 `on_action` 是绑窗口的（多一个 TypeId/Window 参数），
        // 这里要的是应用级全局监听，所以显式走 `App`。
        let cx: &mut App = cx;

        // 切歌 / 播放暂停 / 循环这三个都是「改状态 → 重绘 → 重建菜单」。
        let next = player.clone();
        cx.on_action(move |_: &tray_menu::TrayNext, cx: &mut App| {
            let updated = next
                .update(cx, |player, cx| {
                    player.now.next_track();
                    cx.notify();
                })
                .is_ok();
            if updated {
                tray_menu::refresh(cx);
            }
        });

        let previous = player.clone();
        cx.on_action(move |_: &tray_menu::TrayPrevious, cx: &mut App| {
            let updated = previous
                .update(cx, |player, cx| {
                    player.now.prev_track();
                    cx.notify();
                })
                .is_ok();
            if updated {
                tray_menu::refresh(cx);
            }
        });

        let toggle_play = player.clone();
        cx.on_action(move |_: &tray_menu::TrayTogglePlay, cx: &mut App| {
            let updated = toggle_play
                .update(cx, |player, cx| {
                    // 直接翻转播放状态；右侧面板目前是空的，不需要驱动计时器。
                    player.now.toggle_play();
                    cx.notify();
                })
                .is_ok();
            if updated {
                tray_menu::refresh(cx);
            }
        });

        let toggle_loop = player.clone();
        cx.on_action(move |_: &tray_menu::TrayToggleLoop, cx: &mut App| {
            let updated = toggle_loop
                .update(cx, |player, cx| {
                    player.now.cycle_repeat();
                    cx.notify();
                })
                .is_ok();
            if updated {
                tray_menu::refresh(cx);
            }
        });

        // 点托盘里的「打开主面板」：把窗口提到最前，应用一并激活。
        cx.on_action(|_: &tray_menu::TrayOpenWindow, cx: &mut App| {
            if let Some(window) = cx.windows().first().copied() {
                let _ = window.update(cx, |_, window, _| window.activate_window());
            }
            cx.activate(true);
        });

        // 退出前先摘掉托盘图标，否则 macOS 菜单栏会留一个空位。
        cx.on_action(|_: &tray_menu::TrayQuit, cx: &mut App| {
            tray_menu::close(cx);
            cx.quit();
        });

        // 托盘菜单在构建时会立刻读一次播放状态，而此刻实体还在构造中（被独占借用），
        // 直接装会 panic，所以推到本轮 effect 之后再装。
        let install = player;
        cx.defer(move |cx| {
            if let Err(error) = tray_menu::install(cx, install) {
                eprintln!("创建系统托盘失败: {error}");
            }
        });
    }

    /// 托盘菜单要展示的状态。
    fn tray_model(&self) -> tray_menu::TrayModel {
        tray_menu::TrayModel {
            // 还没接入音频播放，先固定显示「无媒体」；接上后换成当前曲目。
            track: None,
            playing: self.now.playing,
            looping: self.now.repeat.is_on(),
        }
    }

    /// 打开 spotlight：清空上一次输入并把焦点交给搜索框。
    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.search
            .update(cx, |state, cx| state.set_value("", window, cx));
        let focus = self.search.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// 按当前输入过滤目的地（匹配中文名或英文键）。
    fn filtered(&self, cx: &App) -> Vec<Destination> {
        let query = self.search.read(cx).value().to_lowercase();
        Destination::ALL
            .into_iter()
            .filter(|d| {
                query.is_empty()
                    || d.label().to_lowercase().contains(&query)
                    || d.key().contains(&query)
            })
            .collect()
    }

    /// 开始或暂停播放。
    fn set_playing(&mut self, playing: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.now.playing = playing;
        if playing {
            self.start_ticker(window, cx);
        } else {
            // 代次 +1 让正在跑的计时任务在下一拍自行退出。
            self.tick_generation = self.tick_generation.wrapping_add(1);
        }
        cx.notify();
    }

    /// 每 [`TICK_MS`] 推进一次播放进度，驱动进度条与歌词高亮。
    ///
    /// 不保存 `Task`：开始/停止都递增代次号，旧循环发现代次不符就退出，
    /// 这样切换播放状态时不需要手动取消任务。
    fn start_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tick_generation = self.tick_generation.wrapping_add(1);
        let generation = self.tick_generation;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(TICK_MS))
                    .await;
                let alive = this
                    .update(cx, |this, cx| {
                        if this.tick_generation != generation || !this.now.playing {
                            return false;
                        }
                        this.now.advance(TICK_SECS);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// 把当前进度写回进度条状态，避免切歌后拖动从旧位置起算。
    fn sync_slider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let progress = self.now.progress;
        self.seek
            .update(cx, |state, cx| state.set_value(progress, window, cx));
    }

    /// 圆形图标按钮；`active` 表示随机 / 循环这类开关处于开启状态。
    fn round_button(
        &self,
        id: &'static str,
        icon: LucideIcon,
        active: bool,
        diameter: f32,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let theme = UiTheme::read(cx).clone();
        let color = if active {
            theme.colors.foreground
        } else {
            theme.colors.muted_foreground
        };
        let hover_bg = theme.colors.accent;
        let hover_fg = theme.colors.accent_foreground;
        div()
            .id(id)
            .size(px(diameter))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .text_color(color)
            .cursor_pointer()
            .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
            .on_click(cx.listener(move |this, _, window, cx| action(this, window, cx)))
            .child(gpui_icons::lucide(icon).size(px(diameter * 0.5)))
            .into_any_element()
    }

    /// 内容区底部的浮动栏：悬浮在页面之上，高度 20px、圆角 35。
    /// 内容暂时留空，后续接入播放控制。
    fn floating_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::read(cx).clone();

        div()
            .id("content.floatbar")
            .absolute()
            .left(px(FLOAT_BAR_INSET_X))
            .right(px(FLOAT_BAR_INSET_X))
            .bottom(px(FLOAT_BAR_INSET_BOTTOM))
            .h(px(FLOAT_BAR_HEIGHT))
            .flex_shrink_0()
            .rounded(px(FLOAT_BAR_RADIUS))
            .bg(theme.colors.sidebar)
            .border_1()
            .border_color(theme.colors.sidebar_border)
            .shadow(theme.shadows.sm.to_vec())
            .into_any_element()
    }

    /// 构造一行导航项。id 保持稳定，折叠成图标栏时焦点也能保留。
    fn nav_item(
        &self,
        id: &'static str,
        destination: Destination,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = UiTheme::read(cx).clone();
        let selected = self.active == destination;
        let icon_color = if selected {
            theme.colors.sidebar_accent_foreground
        } else {
            theme.colors.sidebar_foreground
        };
        // 图标尺寸与展开态保持一致；图标栏里条目虽然有内边距，但图标溢出的是
        // padding 区域，不会贴到栏边被裁。
        let icon_size = theme.spacing.unit * 5_f32;
        SidebarItem::new(id, destination.label())
            .selected(selected)
            // 库不会自动下发折叠状态，条目必须自己声明，否则图标栏里仍会画出文字。
            .collapsed(collapsed)
            .h(px(42.))
            .icon(
                gpui_icons::lucide(destination.icon())
                    .size(icon_size)
                    .text_color(icon_color),
            )
            .on_activate(cx.listener(move |this, _, _, cx| {
                this.active = destination;
                cx.notify();
            }))
            .into_any_element()
    }

    /// 一个窗口控制按钮（最小化 / 最大化还原 / 关闭）。
    fn window_button(
        &self,
        id: &'static str,
        icon: LucideIcon,
        danger: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let theme = UiTheme::read(cx).clone();
        let s = theme.spacing.unit;
        let fg = theme.colors.sidebar_foreground;
        let hover_bg = theme.colors.sidebar_accent;
        let hover_fg = theme.colors.sidebar_accent_foreground;
        div()
            .id(id)
            .h_full()
            .w(s * 11_f32)
            .flex()
            .items_center()
            .justify_center()
            .text_color(fg)
            // 避免按下按钮时冒泡触发标题栏拖动窗口。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .hover(move |style| {
                if danger {
                    style.bg(gpui_kit::rgb(0xe81123)).text_color(gpui_kit::rgb(0xffffff))
                } else {
                    style.bg(hover_bg).text_color(hover_fg)
                }
            })
            .on_click(move |_, window, app| action(window, app))
            .child(gpui_icons::lucide(icon).size(s * 3.5_f32))
            .into_any_element()
    }

    /// 自绘标题栏：macOS 用系统红绿灯，Windows/Linux 用 lucide 按钮。
    fn titlebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::read(cx).clone();
        let s = theme.spacing.unit;
        let is_mac = cx.platform_info().is_mac();

        // 左侧品牌区：macOS 为红绿灯预留空间；Windows/Linux 直接贴最左端。
        let leading = div()
            .flex()
            .items_center()
            .gap(s * 2_f32)
            .when(is_mac, |el| el.pl(px(78.)))
            .child(
                gpui_icons::lucide(LucideIcon::Music2)
                    .size(s * 4_f32)
                    .text_color(theme.colors.sidebar_foreground),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(s * 1.5_f32)
                    .child(
                        div()
                            .text_size(theme.text(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.colors.sidebar_foreground)
                            .child("Firefly Music"),
                    )
                    .child(
                        // 黄色标签
                        div()
                            .flex()
                            .items_center()
                            .px(s * 1.5_f32)
                            .py(px(2.))
                            .rounded_full()
                            .bg(gpui_kit::rgb(0xfacc15))
                            .text_color(gpui_kit::rgb(0x422006))
                            .text_size(theme.text(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Player"),
                    ),
            );

        // 中央搜索触发框：外观像输入框，点击打开 spotlight。
        let search_hover = theme.colors.sidebar_accent;
        let search_trigger = div()
            .id("titlebar.search")
            .h(px(28.))
            .w(px(320.))
            .min_w_0()
            .flex()
            .items_center()
            .gap(s * 2_f32)
            .px(s * 3_f32)
            .rounded_full()
            .bg(search_hover)
            .border_1()
            .border_color(theme.colors.sidebar_border)
            .text_color(theme.colors.muted_foreground)
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .hover(move |style| style.bg(search_hover.opacity(0.7)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_search(window, cx);
            }))
            .child(
                gpui_icons::lucide(LucideIcon::Search)
                    .size(s * 3.5_f32)
                    .text_color(theme.colors.muted_foreground),
            )
            .child(
                div()
                    .text_size(theme.text(12.5))
                    .child("在这里搜索歌曲 歌手 专辑信息"),
            );

        // 右侧窗口控制按钮（仅 Windows / Linux）。
        let controls: Vec<AnyElement> = if is_mac {
            Vec::new()
        } else {
            let maximize_icon = if window.is_maximized() {
                LucideIcon::Minimize2
            } else {
                LucideIcon::Maximize2
            };
            vec![
                self.window_button(
                    "titlebar.minimize",
                    LucideIcon::Minus,
                    false,
                    cx,
                    |window, _| window.minimize_window(),
                ),
                self.window_button(
                    "titlebar.zoom",
                    maximize_icon,
                    false,
                    cx,
                    |window, _| window.zoom_window(),
                ),
                self.window_button("titlebar.close", LucideIcon::X, true, cx, |_, app| {
                    app.quit()
                }),
            ]
        };

        div()
            .id("app.titlebar")
            .flex()
            .flex_shrink_0()
            .items_center()
            .h(px(TITLEBAR_HEIGHT))
            // 不再画通栏底边：标题栏与侧边栏同色同面，两者之间不留边界线。
            // 内容区上沿的分隔改由页面自己的上边框承担。
            .bg(theme.colors.sidebar)
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                // 双击切换最大化/还原，单击拖动窗口。
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            // 三段式：左右各占一份剩余空间，中间固定宽度，从而精确居中。
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(leading),
            )
            .child(search_trigger)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .children(controls),
            )
            .into_any_element()
    }

    /// 窗口中央的 spotlight 搜索弹层。
    fn spotlight(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::read(cx).clone();
        let s = theme.spacing.unit;
        let results = self.filtered(&*cx);

        let rows: Vec<AnyElement> = if results.is_empty() {
            vec![
                div()
                    .px(s * 3_f32)
                    .py(s * 4_f32)
                    .text_size(theme.text(13.))
                    .text_color(theme.colors.muted_foreground)
                    .child("没有找到匹配的结果")
                    .into_any_element(),
            ]
        } else {
            results
                .into_iter()
                .map(|destination| {
                    let accent = theme.colors.accent;
                    let accent_fg = theme.colors.accent_foreground;
                    div()
                        .id(format!("spotlight.{}", destination.key()))
                        .flex()
                        .items_center()
                        .gap(s * 2.5_f32)
                        .px(s * 3_f32)
                        .py(s * 2.25_f32)
                        .rounded(theme.radius.md)
                        .text_color(theme.colors.popover_foreground)
                        .cursor_pointer()
                        .hover(move |style| style.bg(accent).text_color(accent_fg))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active = destination;
                            this.search_open = false;
                            cx.notify();
                        }))
                        .child(
                            gpui_icons::lucide(destination.icon())
                                .size(s * 4_f32)
                                .text_color(theme.colors.muted_foreground),
                        )
                        .child(
                            div()
                                .text_size(theme.text(13.))
                                .child(destination.label()),
                        )
                        .into_any_element()
                })
                .collect()
        };

        let panel = div()
            .w(px(560.))
            .flex()
            .flex_col()
            .rounded(theme.radius.xl)
            .bg(theme.colors.popover)
            .text_color(theme.colors.popover_foreground)
            .border_1()
            .border_color(theme.colors.border)
            .shadow(theme.shadows.lg.to_vec())
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(s * 2.5_f32)
                    .px(s * 4_f32)
                    .py(s * 3_f32)
                    .child(
                        gpui_icons::lucide(LucideIcon::Search)
                            .size(s * 4.5_f32)
                            .text_color(theme.colors.muted_foreground),
                    )
                    .child(div().flex_1().min_w_0().child(Input::new(&self.search))),
            )
            .child(div().h(px(1.)).bg(theme.colors.border))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .p(s * 2_f32)
                    .children(rows),
            );

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .p(s * 6_f32)
            .bg(theme.colors.overlay)
            .occlude()
            // 点击遮罩关闭；点击面板时阻止冒泡。
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.search_open = false;
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.search_open = false;
                    cx.notify();
                }
            }))
            .child(
                panel.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
            )
            .into_any_element()
    }
}

impl Render for Player {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        // 窄于阈值时自动收缩为图标栏：用派生的布局状态渲染，避免改写用户偏好，
        // 窗口重新变宽后侧边栏会恢复成用户先前选择的状态。
        let narrow = sidebar_narrow(window.viewport_size().width);
        let layout_state = SidebarState {
            open: self.sidebar.open && !narrow,
            mobile_open: self.sidebar.mobile_open,
        };
        // 布局实际处于折叠态（自动收缩或用户手动收起）。
        let collapsed = !layout_state.open;

        // 侧边栏：顶部品牌 + 两组导航，中间用分割线隔开。
        let sidebar = Sidebar::new("player.sidebar", "主导航")
            .header(
                div()
                    .flex()
                    .items_center()
                    .gap(theme.spacing.unit * 2_f32),
            )
            .child(sidebar_menu().gap(theme.spacing.unit).children([
                self.nav_item("nav.home", Destination::Home, collapsed, cx),
                self.nav_item("nav.library", Destination::Library, collapsed, cx),
                self.nav_item("nav.artists", Destination::Artists, collapsed, cx),
                self.nav_item("nav.albums", Destination::Albums, collapsed, cx),
            ]))
            .child(sidebar_separator(cx))
            .child(sidebar_menu().gap(theme.spacing.unit).children([
                self.nav_item("nav.liked", Destination::Liked, collapsed, cx),
                self.nav_item("nav.favorites", Destination::Favorites, collapsed, cx),
            ]))
            // 底部固定项：设置。样式与导航项一致，图标栏时只留图标。
            .footer(self.nav_item("nav.settings", Destination::Settings, collapsed, cx));

        // 右侧页面：由侧边栏当前选中项分发到 `pages::mainwindow` 下对应的页面。
        let content = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.active.page(cx));

        // 内容区在左，页面本身是定位容器，底部叠一条浮动栏。
        // 标题栏已去掉通栏底边，这里用上边框把页面与标题栏分开（侧边栏一侧保持连通）。
        // 左上角 20px 圆角：圆角缺口露出外层外壳色，视觉上像一块嵌进侧边栏里的面板。
        let page = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            .bg(theme.colors.background)
            .rounded_tl(px(PAGE_CORNER_RADIUS))
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.colors.sidebar_border)
            .child(content)
            .child(self.floating_bar(cx));

        // 外壳底色与侧边栏/标题栏一致，作为页面圆角缺口处的底色。
        let content_row = div()
            .size_full()
            .flex()
            .bg(theme.colors.sidebar)
            .child(page);

        // 标题栏容器套在侧边栏（及内容区）之上，横跨窗口整个宽度。
        let mut root = div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme.colors.background)
            .text_color(theme.colors.foreground)
            .font_family(theme.fonts.body.clone())
            .child(self.titlebar(window, cx))
            .child(
                SidebarLayout::new("player.layout", layout_state)
                    .flex_1()
                    .collapsible(SidebarCollapsible::Icon)
                    // 侧边栏与页面之间不画边缘线：两者只靠底色与页面圆角区分。
                    .edge_border(false)
                    .sidebar(sidebar)
                    .content(content_row)
                    .on_change(cx.listener(|this, state: &SidebarState, window, cx| {
                        // 自动收缩期间忽略来自布局的开关变化，保持用户的展开偏好。
                        if sidebar_narrow(window.viewport_size().width) {
                            return;
                        }
                        this.sidebar = *state;
                        cx.notify();
                    })),
            );

        // spotlight 覆盖在窗口中央。
        if self.search_open {
            root = root.child(self.spotlight(cx));
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 阈值边界：950（含）收缩，951 不收缩；最小窗口宽度 900 也落在收缩区间。
    #[test]
    fn sidebar_collapses_at_threshold() {
        assert!(sidebar_narrow(px(MIN_WINDOW_WIDTH)));
        assert!(sidebar_narrow(px(SIDEBAR_COLLAPSE_WIDTH)));
        assert!(!sidebar_narrow(px(SIDEBAR_COLLAPSE_WIDTH + 1.)));
        assert!(!sidebar_narrow(px(1200.)));
    }
}
