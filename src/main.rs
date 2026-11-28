#![allow(dead_code)]

mod audio;
mod pages;
mod services;
mod tray_menu;
mod ui;

use std::cell::Cell;
use std::rc::Rc;

use gpui_icons::{LucideIcon, lucide};
use gpui_kit::base::motion::{
    Easing, Keyframe, Keyframes, MotionStatus, Timing, animate_keyframes,
};
use gpui_kit::base::{Align, DialogHandle, ElementExt as _, Placement, Positioner};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, ElementId, Entity,
    Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ObjectFit, ParentElement as _, Pixels, Render, Rgba, StatefulInteractiveElement as _, Styled,
    StyledImage as _, Window, WindowDecorations, WindowOptions, deferred, div, point,
    prelude::FluentBuilder as _, px, relative, rgb, rgba, size,
};
use ui::{
    app::{AppConfig, run},
    app_theme,
    button::{Button, ButtonSize, ButtonVariant},
    dialog::{dialog_footer, dialog_popup, dialog_title, dialog_with},
    input::{Input, InputEvent, InputState},
    sidebar::{
        Sidebar, SidebarCollapsible, SidebarItem, SidebarLayout, SidebarState, sidebar_menu,
        sidebar_separator,
    },
    theme::{UiTheme, primitives, primitives::surface},
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

// ---- 浮动栏内部（Figma「主窗口内歌词控件」）----
/// 栏身底色（近黑），与设计稿一致。
const FLOAT_BAR_BG: u32 = 0x0018_181B;
/// 封面的边长（圆形）。
const COVER_SIZE: f32 = 45.;
/// 封面左侧留白（设计稿 19px）。
const FLOAT_BAR_PAD_LEFT: f32 = 19.;
/// 最右一颗图标的右侧留白。
const FLOAT_BAR_PAD_RIGHT: f32 = 30.;
/// 封面与文字之间的间距。
const FLOAT_BAR_TEXT_GAP: f32 = 16.;
/// 右侧控制图标的大小。
const BAR_ICON_SIZE: f32 = 20.;
/// 控制按钮的点击区域（正方形，图标居中）。
const BAR_ICON_BUTTON: f32 = 32.;
/// 控制按钮之间的间距（图标中心距 57px）。
const BAR_ICON_GAP: f32 = 25.;
/// 浮动栏上沿的播放进度线厚度。
const PROGRESS_HEIGHT: f32 = 3.;
/// 进度条的点击热区高度：线只有 3px，热区做高一点才好点中。
const PROGRESS_HIT_HEIGHT: f32 = 14.;
/// 浮动栏歌词行的行高；换行滑动量正好是这一行的高度。
const LYRIC_LINE_HEIGHT: f32 = 20.;

/// 音量超过这个比例就先弹一次听力警告。
const VOLUME_WARNING_THRESHOLD: f32 = 0.5;
/// 音量浮层的宽度（标题 + 百分比一行，下面一条滑杆）。
const VOLUME_POPOVER_WIDTH: f32 = 210.;
/// 高音量警告的正文。
const HEARING_WARNING: &str = "过高的音量会损伤您的听力，尤其是在您戴着耳机的情况下，是否继续？";

/// 页面显示区左上角的圆角半径。
const PAGE_CORNER_RADIUS: f32 = 20.;

/// 窗口宽度小于等于该值时，侧边栏自动收缩成图标栏（不覆盖用户的展开偏好）。
const SIDEBAR_COLLAPSE_WIDTH: f32 = 950.;

/// 窗口是否已窄到需要把侧边栏收缩成图标栏。
fn sidebar_narrow(width: gpui_kit::Pixels) -> bool {
    width <= px(SIDEBAR_COLLAPSE_WIDTH)
}

/// 一次音量调整的结果。
#[derive(Clone, Copy, Debug, PartialEq)]
enum VolumeRequest {
    /// 直接生效。
    Apply(f32),
    /// 超过安全线：先按 `applied` 出声，等用户点「继续」再放到 `pending`。
    Warn { applied: f32, pending: f32 },
}

/// 算出滑杆落点该怎么处理。
///
/// 超过一半且本次会话还没确认过，就先弹听力警告；确认过一次之后不再拦。
/// 越界值夹到 `0..=1`，NaN 退回默认音量（否则引擎会被写成 NaN：静音且滑杆乱跳）。
fn resolve_volume_request(value: f32, confirmed: bool) -> VolumeRequest {
    let value = if value.is_finite() {
        value.clamp(0., 1.)
    } else {
        audio::DEFAULT_VOLUME
    };
    if value > VOLUME_WARNING_THRESHOLD && !confirmed {
        VolumeRequest::Warn {
            applied: VOLUME_WARNING_THRESHOLD,
            pending: value,
        }
    } else {
        VolumeRequest::Apply(value)
    }
}

/// 进度条上的点击位置换算成归一化进度（`0..=1`）。
///
/// 轨道宽度还没量出来（首帧之前）或者宽度为 0 时返回 0，避免除零。
fn progress_at(track: &Bounds<Pixels>, x: Pixels) -> f32 {
    let width = track.size.width;
    if width <= px(0.) {
        return 0.;
    }
    ((x - track.left()) / width).clamp(0., 1.)
}

/// 歌词滑动用的关键帧：整块往上挪一行的高度。
///
/// 不能放 `static`：`Keyframes` 内部的缓动是 `Rc`，不是 `Send + Sync`。
/// 每帧重建一次只是一个几十字节的小数组，比缓存更省事。
fn lyric_slide() -> Keyframes<f32> {
    Keyframes::try_new([Keyframe::new(0., 0.), Keyframe::new(1., 1.)])
        .expect("两个端点关键帧总是合法的")
}

/// 浮动栏歌词的换行状态：留住上一行，换行时好做「旧行上移、新行顶上」的过渡。
#[derive(Default)]
struct LyricSwap {
    /// 当前曲目标题；换歌时直接跳到新歌的第一行，不做滑动。
    track: String,
    /// 正在显示的那一行。
    current: String,
    /// 刚刚被顶上去的那一行（滑出动画用）。
    previous: String,
    /// 每次换行自增，作为动画元素 id —— id 变了动画才会重头播。
    generation: u64,
}

impl LyricSwap {
    /// 记下这一帧该显示的内容；返回这一帧是不是刚换了行。
    fn set(&mut self, track: &str, line: &str) -> bool {
        if self.track != track {
            // 换歌：直接从新曲目的第一行开始，不滑一屏旧歌的歌词过去。
            self.track = track.to_string();
            self.current = line.to_string();
            self.previous.clear();
            return false;
        }
        if self.current == line {
            return false;
        }
        self.previous = std::mem::replace(&mut self.current, line.to_string());
        self.generation += 1;
        true
    }
}

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
            Self::Settings => mw::placeholder(LucideIcon::Settings, "设置", "设置页还在建设中", cx),
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
    /// 音量滑块状态（0..=1），与引擎里记录的音量保持一致。
    volume: Entity<SliderState>,
    /// 音量浮层是否展开。
    volume_open: bool,
    /// 本次会话是否已经确认过「高音量伤听力」；确认之后不再反复弹窗。
    volume_confirmed: bool,
    /// 等确认的音量：警告弹窗点「继续」时才真正生效。
    volume_pending: Rc<Cell<f32>>,
    /// 音量按钮的位置（`on_prepaint` 记下来的），用来把浮层贴在按钮上方。
    volume_anchor: Rc<Cell<Bounds<Pixels>>>,
    /// 进度条轨道的位置（`on_prepaint` 记下来的），用来把点击位置换算成进度。
    progress_anchor: Rc<Cell<Bounds<Pixels>>>,
    /// 浮动栏歌词的换行动画状态。
    lyric: LyricSwap,
    /// 高音量警告弹窗。
    volume_warning: DialogHandle,
}

impl Player {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("在这里搜索歌曲、歌手、专辑信息"));
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

        // 音量滑块：拖动就落到引擎上，超过一半先过一道听力警告。
        let volume = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.01));
        cx.subscribe_in(
            &volume,
            window,
            |this, _, event: &SliderEvent, window, cx| {
                if let SliderEvent::Change(value) = event {
                    this.request_volume(value.end(), window, cx);
                }
            },
        )
        .detach();

        // 播放引擎与状态快照都放在全局；计时器也在这里拉起来。
        audio::init(cx);
        // 内嵌封面缓存：音乐库卡片和浮动栏都从这里取图。
        services::covers::init(cx);

        let player = Self {
            active: Destination::Home,
            sidebar: SidebarState::default(),
            search,
            search_open: false,
            volume,
            volume_open: false,
            volume_confirmed: false,
            volume_pending: Rc::new(Cell::new(audio::DEFAULT_VOLUME)),
            volume_anchor: Rc::new(Cell::new(Bounds::default())),
            progress_anchor: Rc::new(Cell::new(Bounds::default())),
            lyric: LyricSwap::default(),
            volume_warning: DialogHandle::new(false),
        };
        // 托盘要在实体存在之后装：菜单动作要回写这个实体。
        player.install_tray(cx);
        // 元数据编辑弹窗的输入框与句柄（音乐库右键菜单用）。
        pages::mainwindow::metadata::init(window, cx);
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

        // 切歌 / 播放暂停 / 循环这三个都是「改播放状态 → 重绘 → 重建菜单」。
        cx.on_action(move |_: &tray_menu::TrayNext, cx: &mut App| {
            audio::next(cx);
            tray_menu::refresh(cx);
        });

        cx.on_action(move |_: &tray_menu::TrayPrevious, cx: &mut App| {
            audio::prev(cx);
            tray_menu::refresh(cx);
        });

        cx.on_action(move |_: &tray_menu::TrayTogglePlay, cx: &mut App| {
            audio::toggle(cx);
            tray_menu::refresh(cx);
        });

        cx.on_action(move |_: &tray_menu::TrayToggleLoop, cx: &mut App| {
            audio::cycle_repeat(cx);
            tray_menu::refresh(cx);
        });

        // 点托盘里的「打开主面板」：把窗口提到最前，应用一并激活。
        cx.on_action(|_: &tray_menu::TrayOpenWindow, cx: &mut App| {
            if let Some(window) = cx.windows().first().copied() {
                let _ = window.update(cx, |_, window, _| window.activate_window());
            }
            cx.activate(true);
        });

        // 「歌词编辑器」：开一个独立窗口，已经开着就前置。
        cx.on_action(|_: &tray_menu::TrayOpenLyricsEditor, cx: &mut App| {
            pages::lyricseditor::window::open(cx);
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
    fn tray_model(&self, cx: &App) -> tray_menu::TrayModel {
        let now = audio::state(cx);
        tray_menu::TrayModel {
            track: now.current().map(|track| {
                let subtitle = track.subtitle();
                if subtitle.is_empty() {
                    track.title.clone()
                } else {
                    format!("{} - {subtitle}", track.title)
                }
            }),
            playing: now.playing,
            looping: now.repeat.is_on(),
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

    /// 浮动栏的音量按钮；点一下开关音量浮层。
    ///
    /// 图标跟着音量变（静音是 `VolumeX`），浮层展开时底色常亮，和循环按钮的高亮语汇一致。
    fn volume_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let volume = audio::volume(cx);
        let open = self.volume_open;
        let icon = if volume <= 0.001 {
            LucideIcon::VolumeX
        } else {
            LucideIcon::Volume2
        };
        let color = if open {
            rgb(0x00FF_FFFF)
        } else {
            rgba(0xFFFFFF8A)
        };
        // 浮层要贴在按钮上方，位置只能等布局算完才知道，先记下来。
        let anchor = self.volume_anchor.clone();

        div()
            .id("bar.volume")
            .debug_selector(|| "bar.volume".into())
            .aria_label("音量")
            .size(px(BAR_ICON_BUTTON))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .when(open, |element| element.bg(rgba(0xFFFFFF24)))
            .hover(|style| style.bg(rgba(0xFFFFFF14)))
            .on_prepaint(move |bounds, _, _| anchor.set(bounds))
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_volume(window, cx)),
            )
            .child(lucide(icon).size(px(BAR_ICON_SIZE)).text_color(color))
            .into_any_element()
    }

    /// 开 / 收音量浮层；展开时把滑杆对齐到当前音量。
    fn toggle_volume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.volume_open = !self.volume_open;
        if self.volume_open {
            let current = audio::volume(cx);
            self.volume
                .update(cx, |state, cx| state.set_value(current, window, cx));
        }
        cx.notify();
    }

    /// 滑杆落点：超过一半要先过听力警告，确认之前声音只放到一半。
    ///
    /// 先压住音量再问，是为了不让「警告还没读完，耳朵先被轰一下」这件事发生。
    fn request_volume(&mut self, value: f32, window: &mut Window, cx: &mut Context<Self>) {
        match resolve_volume_request(value, self.volume_confirmed) {
            VolumeRequest::Apply(value) => audio::set_volume(cx, value),
            VolumeRequest::Warn { applied, pending } => {
                audio::set_volume(cx, applied);
                self.volume_pending.set(pending);
                self.volume
                    .update(cx, |state, cx| state.set_value(applied, window, cx));
                self.volume_warning.open(window, cx);
            }
        }
    }

    /// 用户在听力警告里点了「继续」：接受这次的音量，并记住本次会话已确认。
    fn accept_loud_volume(&mut self, cx: &mut Context<Self>) {
        let value = self.volume_pending.get();
        self.volume_confirmed = true;
        audio::set_volume(cx, value);
        cx.notify();
    }

    /// 音量浮层：一行「音量 + 百分比」，下面一条滑杆。
    ///
    /// 用 `deferred` + `Positioner` 挂在窗口最上层（和菜单同一条路子），
    /// 免得被浮动栏的圆角与页面裁剪掉。
    fn volume_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::read(cx).clone();
        let volume = audio::volume(cx);
        let label = if volume <= 0.001 {
            "静音".to_string()
        } else {
            format!("{:.0}%", volume * 100.)
        };

        let panel = surface::popover(
            div()
                .id("bar.volume.panel")
                .debug_selector(|| "bar.volume-panel".into())
                .w(px(VOLUME_POPOVER_WIDTH))
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(14.))
                // 点到浮层以外就收起（和菜单一致）。
                // 点在音量按钮上要放过：那一次交给按钮自己 toggle，
                // 否则「按下先关、抬起又开」看起来像没反应。
                .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if this.volume_anchor.get().contains(&event.position) {
                        return;
                    }
                    if this.volume_open {
                        this.volume_open = false;
                        cx.notify();
                    }
                })),
            &theme,
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.colors.popover_foreground)
                        .child("音量"),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.colors.muted_foreground)
                        .child(label),
                ),
        )
        .child(
            Slider::new(&self.volume)
                .horizontal()
                .w_full()
                .accessibility_label("音量"),
        );

        deferred(
            Positioner::side(self.volume_anchor.get())
                .placement(Placement::Top)
                .align(Align::End)
                .offset(px(8.))
                .margin(px(8.))
                .occlude()
                .child(panel),
        )
        .with_priority(gpui_kit::base::POPUP_PRIORITY)
        .into_any_element()
    }

    /// 高音量警告弹窗（挂在窗口最外层，和元数据弹窗同一层）。
    fn volume_warning_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let handle = self.volume_warning.clone();
        let slider = self.volume.clone();
        let this = cx.entity();

        dialog_with(
            "volume-warning",
            &self.volume_warning,
            move |_window, cx| {
                let theme = UiTheme::read(cx).clone();

                let cancel_handle = handle.clone();
                let cancel_slider = slider.clone();
                let confirm_handle = handle.clone();
                let confirm_slider = slider.clone();
                let confirm_this = this.clone();

                dialog_popup("volume-warning-popup", "音量过高", cx)
                    .debug_selector(|| "volume-warning-popup".into())
                    .max_w(px(460.))
                    .child(dialog_title("volume-warning-title", cx).child("音量过高"))
                    .child(
                        div()
                            .w_full()
                            .text_size(px(13.))
                            .line_height(px(21.))
                            .text_color(theme.colors.muted_foreground)
                            .child(HEARING_WARNING),
                    )
                    .child(
                        dialog_footer(cx)
                            .child(
                                Button::new("volume-warning-cancel")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Sm)
                                    .child("取消")
                                    .on_click(move |_, window, cx| {
                                        // 停在安全音量上，滑杆也一起退回去。
                                        let safe = audio::volume(cx).min(VOLUME_WARNING_THRESHOLD);
                                        cancel_slider.update(cx, |state, cx| {
                                            state.set_value(safe, window, cx)
                                        });
                                        cancel_handle.close(window, cx);
                                    }),
                            )
                            .child(
                                Button::new("volume-warning-continue")
                                    .variant(ButtonVariant::Default)
                                    .size(ButtonSize::Sm)
                                    .child("继续")
                                    .on_click(move |_, window, cx| {
                                        confirm_this
                                            .update(cx, |this, cx| this.accept_loud_volume(cx));
                                        let value = confirm_this.read(cx).volume_pending.get();
                                        confirm_slider.update(cx, |state, cx| {
                                            state.set_value(value, window, cx)
                                        });
                                        confirm_handle.close(window, cx);
                                    }),
                            ),
                    )
                    .into_any_element()
            },
            window,
            cx,
        )
        .into_any_element()
    }

    /// 内容区底部的浮动歌词控件（Figma「主窗口内歌词控件」）。
    ///
    /// 从左到右：圆形封面 → 当前歌词 + 曲目信息 → 循环 / 音量 / 上一曲 / 播放暂停 / 下一曲。
    /// 深色胶囊底板是设计稿定死的，不跟随浅色主题，和页面形成对比。
    fn floating_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::read(cx).clone();
        let now = audio::bar_view(cx);
        // 先把这一帧的歌词记进换行状态：换了行下一次绘制就带着上滑动画。
        self.lyric.set(&now.title, &now.lyric);
        // 出错时优先把原因说出来，比显示时间有用。
        let detail = match now.error.as_deref() {
            Some(error) => error.to_string(),
            None => {
                let mut line = format!("《{}》", now.title);
                if !now.subtitle.is_empty() {
                    line.push_str(&format!(" - {}", now.subtitle));
                }
                line.push_str(&format!(" · {}/{}", now.elapsed, now.duration));
                line
            }
        };

        // 有内嵌封面就画图，没有就退回音符占位。
        // 缓存里没有时会顺手排进后台队列，读回来重绘一次这里就能拿到图。
        let artwork = now
            .path
            .as_deref()
            .and_then(|path| services::covers::thumb(cx, path));
        let cover = match artwork {
            Some(source) => div()
                .size(px(COVER_SIZE))
                .flex_shrink_0()
                // 圆形裁切：图片是方的，不裁会露出四个角。
                .rounded_full()
                .overflow_hidden()
                .border_1()
                .border_color(rgba(0xFFFFFF24))
                .child(
                    primitives::image(source)
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                ),
            None => div()
                .size(px(COVER_SIZE))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(0x002E_2E35))
                .border_1()
                .border_color(rgba(0xFFFFFF24))
                .child(
                    lucide(LucideIcon::Music)
                        .size(px(18.))
                        .text_color(rgba(0xFFFFFFB8)),
                ),
        };

        let text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(self.lyric_line(&theme, window, cx))
            .child(
                div()
                    .truncate()
                    .text_size(px(13.))
                    .text_color(rgba(0xFFFFFFB0))
                    .child(detail),
            );

        // 循环图标：关 → 灰，列表循环 / 单曲循环 → 白色高亮。
        let repeat_color = if now.repeat.is_on() {
            rgb(0x00FF_FFFF)
        } else {
            rgba(0xFFFFFF8A)
        };
        let playing = now.playing;

        let controls = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(BAR_ICON_GAP))
            .child(bar_icon_button(
                "bar.repeat",
                LucideIcon::Repeat1,
                repeat_color,
                cx.listener(|_, _: &ClickEvent, _window, cx| {
                    audio::cycle_repeat(cx);
                    // 托盘菜单重建要读播放状态，推到本轮 effect 之后再做。
                    cx.defer(|cx| tray_menu::refresh(cx));
                }),
            ))
            .child(self.volume_button(cx))
            .child(bar_icon_button(
                "bar.prev",
                LucideIcon::SkipBack,
                rgb(0x00FF_FFFF),
                cx.listener(|_, _: &ClickEvent, _window, cx| {
                    audio::prev(cx);
                    cx.defer(|cx| tray_menu::refresh(cx));
                }),
            ))
            .child(bar_icon_button(
                "bar.play",
                if playing {
                    LucideIcon::Pause
                } else {
                    LucideIcon::Play
                },
                rgb(0x00FF_FFFF),
                cx.listener(|_, _: &ClickEvent, _window, cx| {
                    audio::toggle(cx);
                    cx.defer(|cx| tray_menu::refresh(cx));
                }),
            ))
            .child(bar_icon_button(
                "bar.next",
                LucideIcon::SkipForward,
                rgb(0x00FF_FFFF),
                cx.listener(|_, _: &ClickEvent, _window, cx| {
                    audio::next(cx);
                    cx.defer(|cx| tray_menu::refresh(cx));
                }),
            ));

        div()
            .id("content.floatbar")
            .absolute()
            .left(px(FLOAT_BAR_INSET_X))
            .right(px(FLOAT_BAR_INSET_X))
            .bottom(px(FLOAT_BAR_INSET_BOTTOM))
            .h(px(FLOAT_BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(FLOAT_BAR_TEXT_GAP))
            .pl(px(FLOAT_BAR_PAD_LEFT))
            .pr(px(FLOAT_BAR_PAD_RIGHT))
            .rounded(px(FLOAT_BAR_RADIUS))
            .bg(rgb(FLOAT_BAR_BG))
            .border_1()
            .border_color(rgba(0xFFFFFF14))
            .shadow(theme.shadows.md.to_vec())
            .child(cover)
            .child(text)
            .child(controls)
            // 进度线贴在胶囊上沿，放在最后画，压住边框那一像素。
            .when(now.loaded, |bar| {
                bar.child(self.progress_line(now.progress, cx))
            })
            // 浮层用 deferred 画在窗口最上层，不参与栏内的弹性布局。
            .when(self.volume_open, |bar| bar.child(self.volume_popover(cx)))
            .into_any_element()
    }

    /// 浮动栏上沿的播放进度线：细线贴着胶囊顶边的直段，点任意位置跳到对应时刻。
    ///
    /// 胶囊圆角半径等于半高，所以上沿的直段正好从 `FLOAT_BAR_RADIUS` 处开始；
    /// 进度线左右各让出一个半径，避免画到弧线上「戳」出圆角。
    fn progress_line(&self, progress: f32, cx: &mut Context<Self>) -> AnyElement {
        let anchor = self.progress_anchor.clone();
        let progress = progress.clamp(0., 1.);

        div()
            .id("bar.progress")
            .debug_selector(|| "bar.progress".into())
            .aria_label("播放进度")
            .absolute()
            .top(px(0.))
            .left(px(FLOAT_BAR_RADIUS))
            .right(px(FLOAT_BAR_RADIUS))
            .h(px(PROGRESS_HIT_HEIGHT))
            .flex()
            .items_start()
            .cursor_pointer()
            .on_prepaint(move |bounds, _, _| anchor.set(bounds))
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.seek_to(event.position().x, cx);
            }))
            .child(
                div()
                    .w_full()
                    .h(px(PROGRESS_HEIGHT))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(rgba(0xFFFFFF38))
                    .child(
                        div()
                            .h_full()
                            .w(relative(progress))
                            .rounded_full()
                            .bg(rgb(0x00FF_FFFF)),
                    ),
            )
            .into_any_element()
    }

    /// 进度条上点一下：按点击位置在轨道里的比例跳转。
    fn seek_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let progress = progress_at(&self.progress_anchor.get(), x);
        audio::seek_progress(cx, progress);
    }

    /// 浮动栏第一行：当前歌词。
    ///
    /// 换行时把上一行和新行上下排在一起，整块往上滑一行的高度：
    /// 旧行从框的上沿退出去，新行从下面顶上来。直接换字太生硬。
    /// 系统或主题开了「减少动态效果」、以及换歌（`previous` 被清空）时不做过渡。
    fn lyric_line(
        &mut self,
        theme: &UiTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let line = |id: &'static str, text: &str| {
            div()
                .debug_selector(move || id.into())
                .h(px(LYRIC_LINE_HEIGHT))
                .flex()
                .items_center()
                .child(
                    div()
                        // 行是 flex 容器，文字要自己撑满一行才会正确省略。
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(15.))
                        // 行高钉死成行高：否则框按字体的自然行高裁，上下会被切掉一点。
                        .line_height(px(LYRIC_LINE_HEIGHT))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(0x00FF_FFFF))
                        .child(text.to_string()),
                )
        };

        if self.lyric.previous.is_empty() || theme.motion.reduced || cx.reduce_motion() {
            return line("lyric-line", &self.lyric.current).into_any_element();
        }

        // 一次性 0→1 的进度：id 里带代次，换一行就换一个 id，动画才会从头再播。
        let frames = lyric_slide();
        let slide = animate_keyframes(
            ElementId::named_usize("lyric-swap", self.lyric.generation as usize),
            &frames,
            Timing::new(theme.motion.normal).ease(Easing::EaseInOut),
            window,
            cx,
        );
        // 滑完就把旧行丢掉：它已经在框外，留着只会让读屏软件把歌词念两遍。
        if slide.status == MotionStatus::Finished {
            self.lyric.previous.clear();
            return line("lyric-line", &self.lyric.current).into_any_element();
        }

        let progress = slide.value.clamp(0., 1.);
        let swap = &self.lyric;

        div()
            .h(px(LYRIC_LINE_HEIGHT))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(line("lyric-previous", &swap.previous))
                    .child(line("lyric-current", &swap.current))
                    .mt(px(-LYRIC_LINE_HEIGHT * progress)),
            )
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
                    style
                        .bg(gpui_kit::rgb(0xe81123))
                        .text_color(gpui_kit::rgb(0xffffff))
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
                self.window_button("titlebar.zoom", maximize_icon, false, cx, |window, _| {
                    window.zoom_window()
                }),
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
                        .child(div().text_size(theme.text(13.)).child(destination.label()))
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.search_open = false;
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.search_open = false;
                    cx.notify();
                }
            }))
            .child(panel.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()))
            .into_any_element()
    }
}

/// 浮动栏右侧的图标按钮：32px 圆形热区，悬停时叠一层淡白，图标本身保持设计稿的颜色。
fn bar_icon_button(
    id: &'static str,
    icon: LucideIcon,
    color: Rgba,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .size(px(BAR_ICON_BUTTON))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF14)))
        .child(lucide(icon).size(px(BAR_ICON_SIZE)).text_color(color))
        .on_click(on_click)
        .into_any_element()
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
            .header(div().flex().items_center().gap(theme.spacing.unit * 2_f32))
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
            .child(self.floating_bar(window, cx));

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

        // 元数据编辑弹窗：挂最外层，盖住整个窗口。
        root = root.child(pages::mainwindow::metadata::dialog(window, cx));

        // 高音量警告弹窗，同样挂最外层。
        root = root.child(self.volume_warning_dialog(window, cx));

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

    /// 50% 以内直接生效；超过 50% 先压到 50% 并等确认；确认过就不再拦。
    #[test]
    fn volume_warns_only_above_half_until_confirmed() {
        assert_eq!(resolve_volume_request(0., false), VolumeRequest::Apply(0.));
        assert_eq!(
            resolve_volume_request(VOLUME_WARNING_THRESHOLD, false),
            VolumeRequest::Apply(VOLUME_WARNING_THRESHOLD),
            "正好一半不算「过高」"
        );
        assert_eq!(
            resolve_volume_request(0.51, false),
            VolumeRequest::Warn {
                applied: VOLUME_WARNING_THRESHOLD,
                pending: 0.51
            },
            "过线先按一半出声，等用户确认"
        );
        assert_eq!(
            resolve_volume_request(0.8, true),
            VolumeRequest::Apply(0.8),
            "点过「继续」之后不再反复弹窗"
        );
        assert_eq!(
            resolve_volume_request(-1., false),
            VolumeRequest::Apply(0.),
            "越界值夹到 0"
        );
        assert_eq!(
            resolve_volume_request(2., false),
            VolumeRequest::Warn {
                applied: VOLUME_WARNING_THRESHOLD,
                pending: 1.
            },
            "超过 1 的值夹到 1，仍要警告"
        );
        assert_eq!(
            resolve_volume_request(f32::NAN, false),
            VolumeRequest::Apply(audio::DEFAULT_VOLUME),
            "NaN 不能写进引擎"
        );
    }

    /// 进度条的点击位置按轨道宽度换算，越界夹到两端。
    #[test]
    fn progress_point_maps_into_the_track() {
        let track = Bounds::new(point(px(100.), px(0.)), size(px(200.), px(14.)));
        assert_eq!(progress_at(&track, px(100.)), 0., "左端是 0");
        assert_eq!(progress_at(&track, px(300.)), 1., "右端是 1");
        assert!(
            (progress_at(&track, px(150.)) - 0.25).abs() < 1e-6,
            "1/4 处应是 0.25"
        );
        assert_eq!(progress_at(&track, px(50.)), 0., "轨道左边按下去夹到 0");
        assert_eq!(progress_at(&track, px(900.)), 1., "轨道右边按下去夹到 1");

        let unmeasured = Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.)));
        assert_eq!(
            progress_at(&unmeasured, px(10.)),
            0.,
            "还没量出宽度时不能除零"
        );
    }

    /// 换行才记上一行、才换代次；同一行重复绘制不该反复重启动画。
    #[test]
    fn lyric_swap_remembers_the_line_it_just_left() {
        let mut swap = LyricSwap::default();
        assert!(!swap.set("歌名", "第一行"), "第一次只记下来，不放动画");
        assert!(!swap.set("歌名", "第一行"), "同一行不该重复触发动效");
        assert_eq!(swap.generation, 0);

        assert!(swap.set("歌名", "第二行"), "换行应触发动效");
        assert_eq!(swap.previous, "第一行");
        assert_eq!(swap.current, "第二行");
        assert_eq!(swap.generation, 1, "代次变了动画元素 id 才变，动画才会重播");

        assert!(swap.set("歌名", "第三行"));
        assert_eq!(swap.previous, "第二行", "只保留紧邻的上一行");
        assert_eq!(swap.generation, 2);
    }

    /// 换歌不该把上一首的歌词滑出去：直接落到新歌的第一行。
    #[test]
    fn lyric_swap_does_not_slide_across_tracks() {
        let mut swap = LyricSwap::default();
        swap.set("A", "A 的第一行");
        assert!(swap.set("A", "A 的第二行"));
        let generation = swap.generation;

        assert!(
            !swap.set("B", "B 的第一行"),
            "换歌直接跳过去，不滑上一首的歌词"
        );
        assert_eq!(swap.previous, "", "上一行要清掉，界面才不会画滑动层");
        assert_eq!(swap.current, "B 的第一行");
        assert_eq!(swap.generation, generation, "换歌不必换代次");
    }

    /// 歌词换行时旧行和新行上下排好再滑，不是原地换字。
    #[gpui_kit::test]
    fn a_lyric_change_slides_the_old_line_out(cx: &mut gpui_kit::TestAppContext) {
        cx.update(ui::theme::init);
        let (player, cx) = cx.add_window_view(|window, cx| Player::new(window, cx));

        let play = |cx: &mut gpui_kit::VisualTestContext, title: &'static str, position: f32| {
            cx.update(|_, cx| {
                let mut now = audio::state(cx);
                now.queue = vec![ui::now_playing::Track::new(
                    std::path::PathBuf::from(format!("/music/{title}.mp3")),
                    title,
                )];
                now.index = 0;
                now.position = position;
                now.duration = 120.;
                now.lyrics = ui::now_playing::parse_lrc("[00:00.00]第一行\n[00:30.00]第二行");
                now.error = None;
                cx.set_global(now);
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
        };

        play(cx, "测试", 0.);
        assert!(cx.debug_bounds("lyric-line").is_some(), "起手只有一行");
        assert!(
            cx.debug_bounds("lyric-current").is_none(),
            "还没有上一行，不该出现滑动层"
        );

        // 播到第二行：旧行留在上面，新行排在下面等着被顶上来。
        play(cx, "测试", 31.);
        let previous = cx.debug_bounds("lyric-previous").expect("换行后应保留上一行");
        let current = cx.debug_bounds("lyric-current").expect("换行后应画新行");
        assert_eq!(
            previous.size.height,
            px(LYRIC_LINE_HEIGHT),
            "滑动层一行正好一个行高"
        );
        assert!(
            current.top() > previous.top(),
            "新行应在旧行下方：{previous:?} / {current:?}"
        );
        assert!(
            (current.top() - previous.top() - px(LYRIC_LINE_HEIGHT)).abs() < px(0.5),
            "两行应正好差一个行高"
        );

        // 动画得真的往前走：把模拟时钟推过去再画一帧，新行应该已经往上挪了。
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(90));
        player.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let moved = cx.debug_bounds("lyric-current").expect("换行后应画新行");
        assert!(
            moved.top() < current.top() - px(1.),
            "动画应让新行往上走：第一帧 {:?} → 现在 {:?}",
            current,
            moved
        );

        // 动画走完：整块正好挪了一行，旧行被丢掉，回到单行布局。
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(400));
        player.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("lyric-current").is_none(),
            "滑完之后不该再留滑动层"
        );
        assert!(
            cx.debug_bounds("lyric-previous").is_none(),
            "滑完之后旧行应被丢掉，只留一行"
        );
        let settled = cx.debug_bounds("lyric-line").expect("滑完回到单行布局");
        assert!(
            (settled.top() - (current.top() - px(LYRIC_LINE_HEIGHT))).abs() < px(0.5),
            "结束时应正好滑过一行：第一帧 {:?} → 现在 {:?}",
            current,
            settled
        );

        // 换歌：不把上一首的歌词滑出去。
        cx.update(|_, cx| {
            let mut now = audio::state(cx);
            now.queue = vec![ui::now_playing::Track::new(
                std::path::PathBuf::from("/music/另一首.mp3"),
                "另一首",
            )];
            now.index = 0;
            now.position = 0.;
            now.lyrics.clear();
            cx.set_global(now);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("lyric-line").is_some(), "换歌后只画一行");
        assert!(
            cx.debug_bounds("lyric-current").is_none(),
            "换歌不该滑上一首的歌词"
        );
    }

    /// 有曲目时上沿出现进度条，点 1/4 处跳到这首歌的 1/4。
    #[gpui_kit::test]
    fn clicking_the_progress_line_seeks_to_that_point(cx: &mut gpui_kit::TestAppContext) {
        cx.update(ui::theme::init);
        let (_, cx) = cx.add_window_view(|window, cx| Player::new(window, cx));

        // 造一份「正在放一首 200 秒的歌」的状态：不出声，只验证进度条与跳转。
        cx.update(|_, cx| {
            let mut now = audio::state(cx);
            now.queue = vec![ui::now_playing::Track::new(
                std::path::PathBuf::from("/music/测试.mp3"),
                "测试",
            )];
            now.index = 0;
            now.position = 0.;
            now.duration = 200.;
            now.error = None;
            cx.set_global(now);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let track = cx
            .debug_bounds("bar.progress")
            .expect("有曲目时浮动栏上沿应有进度条");
        assert_eq!(
            track.size.height,
            px(PROGRESS_HIT_HEIGHT),
            "进度条的点击热区应比线本身高"
        );
        assert!(
            track.left() >= px(FLOAT_BAR_RADIUS),
            "进度条不该压在胶囊左上角的弧线上：{track:?}"
        );

        let at = point(track.left() + track.size.width * 0.25, track.center().y);
        cx.simulate_click(at, Default::default());

        let position = cx.update(|_, cx| audio::state(cx).position);
        assert!(
            (position - 50.).abs() < 0.6,
            "点 1/4 处应跳到 50 秒，实际 {position}"
        );
    }

    /// 已经播到一半时，点进度条上「前面」的位置要能退回去。
    ///
    /// 之前只能往后点：解码器拿不到文件长度，往回 seek 报 `ForwardOnly`。
    #[gpui_kit::test]
    fn clicking_earlier_on_the_progress_line_seeks_back(cx: &mut gpui_kit::TestAppContext) {
        cx.update(ui::theme::init);
        let (_, cx) = cx.add_window_view(|window, cx| Player::new(window, cx));
        cx.update(|_, cx| {
            let mut now = audio::state(cx);
            now.queue = vec![ui::now_playing::Track::new(
                std::path::PathBuf::from("/music/测试.mp3"),
                "测试",
            )];
            now.index = 0;
            now.position = 0.;
            now.duration = 200.;
            now.error = None;
            cx.set_global(now);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let track = cx.debug_bounds("bar.progress").expect("进度条");

        // 先点到 3/4。
        let at = point(track.left() + track.size.width * 0.75, track.center().y);
        cx.simulate_click(at, Default::default());
        let fwd = cx.update(|_, cx| audio::state(cx).position);
        assert!((fwd - 150.).abs() < 0.6, "点 3/4 应到 150 秒，实际 {fwd}");

        cx.update(|window, cx| window.draw(cx).clear(cx));
        let track = cx.debug_bounds("bar.progress").expect("进度条");

        // 再往回到 1/4。
        let at = point(track.left() + track.size.width * 0.25, track.center().y);
        cx.simulate_click(at, Default::default());
        let back = cx.update(|_, cx| audio::state(cx).position);
        assert!((back - 50.).abs() < 0.6, "回到 1/4 应到 50 秒，实际 {back}");
    }

    /// 音量按钮 → 浮层 → 听力警告的整条路径。
    #[gpui_kit::test]
    fn volume_button_opens_the_popover_and_warns_above_half(cx: &mut gpui_kit::TestAppContext) {
        cx.update(ui::theme::init);
        let (view, cx) = cx.add_window_view(|window, cx| Player::new(window, cx));

        // 点一下展开，再点一下收起。
        let button = cx.debug_bounds("bar.volume").expect("浮动栏里应有音量按钮");
        cx.simulate_click(button.center(), Default::default());
        let panel = cx
            .debug_bounds("bar.volume-panel")
            .expect("点音量按钮应展开浮层");
        assert!(
            panel.bottom() <= button.top(),
            "浮层应贴在按钮上方：panel={panel:?} button={button:?}"
        );
        assert!(
            (panel.right() - button.right()).abs() < px(2.),
            "浮层应与按钮右边缘对齐：panel={panel:?} button={button:?}"
        );
        cx.simulate_click(button.center(), Default::default());
        assert!(
            cx.debug_bounds("bar.volume-panel").is_none(),
            "再点一次应收起浮层"
        );

        // 拖到 80%：先压回安全音量，再弹听力警告。
        cx.update(|window, cx| view.update(cx, |this, cx| this.request_volume(0.8, window, cx)));
        assert_eq!(
            cx.update(|_, cx| audio::volume(cx)),
            VOLUME_WARNING_THRESHOLD,
            "确认之前声音只能到 50%"
        );
        assert!(
            cx.update(|_, cx| view.read(cx).volume_warning.is_open()),
            "超过 50% 应先弹听力警告"
        );
        let dialog = cx
            .debug_bounds("volume-warning-popup")
            .expect("警告弹窗应该真的画出来");
        assert!(
            dialog.size.width > px(200.) && dialog.size.height > px(80.),
            "警告弹窗尺寸不合常理：{dialog:?}"
        );

        // 点「继续」：这次的值才真正生效，且本次会话不再拦。
        cx.update(|_, cx| view.update(cx, |this, cx| this.accept_loud_volume(cx)));
        assert_eq!(cx.update(|_, cx| audio::volume(cx)), 0.8);
        cx.update(|window, cx| view.update(cx, |this, cx| this.request_volume(0.9, window, cx)));
        assert_eq!(
            cx.update(|_, cx| audio::volume(cx)),
            0.9,
            "确认过之后不再反复弹窗"
        );
    }
}
