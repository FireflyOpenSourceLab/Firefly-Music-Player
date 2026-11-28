//! 音乐库。
//!
//! 页面结构：左上角标题「音乐库」，下面是内容区。
//!
//! - 没有添加文件夹时：空状态占位（「还没有音乐哦」+「添加文件夹」按钮）。
//! - 添加后有左右两栏：左栏切换文件夹，右栏列出该文件夹里识别到的音频文件。
//!
//! 扫描在后台线程做（`cx.background_executor`），用 `generation` 代次丢弃过期结果，
//! 避免切文件夹时慢扫描把旧结果写回来。

use std::path::{Path, PathBuf};

use crate::{
    audio,
    services::covers,
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        menu::{Menu, MenuItem, MenuState},
        now_playing::Track,
        theme::{UiTheme, primitives},
    },
};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Entity, FontWeight, Global,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, PathPromptOptions,
    StatefulInteractiveElement as _, Styled, StyledImage as _, Window, div,
    prelude::FluentBuilder as _, px,
};

/// 页头与内容区的左右留白。
const PAGE_PADDING_X: f32 = 32.;
/// 底部要给浮动播放栏（70px 高 + 12px 间距）让出的高度。
const BOTTOM_CLEARANCE: f32 = 100.;
/// 左栏（文件夹列表）宽度。
const FOLDER_PANE_WIDTH: f32 = 208.;
/// 文件夹行的行高。
const FOLDER_ROW_HEIGHT: f32 = 34.;
/// 歌曲卡片的行高。
const TRACK_ROW_HEIGHT: f32 = 64.;
/// 歌曲卡片的圆角。
const TRACK_CARD_RADIUS: f32 = 35.;
/// 卡片内左右留白与列间距。
const TRACK_CARD_PADDING: f32 = 16.;
const TRACK_COLUMN_GAP: f32 = 12.;
/// 卡片最左侧的圆形播放按钮直径。
const TRACK_PLAY_SIZE: f32 = 28.;
/// 封面缩略图边长与圆角。
const TRACK_COVER_SIZE: f32 = 40.;
const TRACK_COVER_RADIUS: f32 = 6.;
/// 右对齐的固定列宽：专辑 / 操作（收藏）/ 时长 / 大小。
const COL_ALBUM: f32 = 220.;
const COL_ACTION: f32 = 56.;
const COL_DURATION: f32 = 64.;
const COL_SIZE: f32 = 80.;
/// 表头行高。
const TRACK_HEADER_HEIGHT: f32 = 28.;
/// 单次扫描最多收这么多首，防止误选整个磁盘目录时卡死内存。
const MAX_TRACKS: usize = 5000;
/// 递归扫描的最大深度。
const MAX_DEPTH: usize = 8;

/// 识别为音乐的扩展名（小写）。
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "flac", "m4a", "aac", "ogg", "opus", "aiff", "aif", "ape", "wma",
];

/// 扫描到的一首音频文件。
#[derive(Clone, Debug, PartialEq)]
pub struct AudioFile {
    /// 文件完整路径。
    pub path: PathBuf,
    /// 展示用标题（文件名去掉扩展名；`艺人 - 曲名` 形式会拆开）。
    pub title: String,
    /// 艺人：优先取文件标签，没有才从文件名推断。
    pub artist: Option<String>,
    /// 专辑：只有文件标签里带了的才有。
    pub album: Option<String>,
    /// 小写扩展名。
    pub extension: String,
    /// 文件大小（字节）。
    pub size: u64,
}

/// 音乐库状态，存在全局里供无状态的页面函数读取。
#[derive(Clone, Debug, Default)]
pub struct LibraryState {
    /// 用户添加的文件夹，按添加顺序去重。
    pub folders: Vec<PathBuf>,
    /// 左栏当前选中的文件夹下标。
    pub selected: usize,
    /// 选中文件夹里扫描到的音频文件。
    pub files: Vec<AudioFile>,
    /// 每张歌曲卡片各自的右键菜单状态，与 `files` 同序。
    pub menus: Vec<Entity<MenuState>>,
    /// 是否正在扫描。
    pub scanning: bool,
    /// 扫描代次：每次换文件夹 / 追加文件夹都 +1，旧扫描结果据此丢弃。
    pub generation: u64,
}

impl Global for LibraryState {}

impl LibraryState {
    /// 当前选中的文件夹。
    fn current(&self) -> Option<&Path> {
        self.folders.get(self.selected).map(PathBuf::as_path)
    }
}

/// 读取一份状态快照；没初始化过就是空的。
pub(crate) fn state(cx: &App) -> LibraryState {
    cx.try_global::<LibraryState>().cloned().unwrap_or_default()
}

/// 改全局状态：先取快照，改完写回，省得外面到处 `set_global`。
fn update(cx: &mut App, f: impl FnOnce(&mut LibraryState)) {
    let mut next = state(cx);
    f(&mut next);
    cx.set_global(next);
}

/// 渲染音乐库页。
pub fn view(cx: &mut App) -> AnyElement {
    let theme = UiTheme::read(cx).clone();
    let state = state(cx);
    // 播放状态用来在列表里标出「正在放的那首」。
    let now = audio::list_mark(cx);

    let mut root = div()
        .size_full()
        .flex()
        .flex_col()
        .text_color(theme.colors.foreground)
        .font_family(theme.fonts.body.clone())
        .child(header(&theme, &state));

    if state.folders.is_empty() {
        root = root.child(empty_state(&theme));
    } else {
        root = root.child(library_body(&state, &theme, &now, cx));
    }

    root.into_any_element()
}

/// 页头：左上角的页面标题，右侧显示文件夹数量。
fn header(theme: &UiTheme, state: &LibraryState) -> AnyElement {
    let mut header = div()
        .flex_shrink_0()
        .px(px(PAGE_PADDING_X))
        .pt(px(26.))
        .pb(px(18.))
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("音乐库"),
        );

    if !state.folders.is_empty() {
        header = header.child(
            div()
                .text_size(theme.text(12.))
                .text_color(theme.colors.muted_foreground)
                .child(format!("{} 个文件夹", state.folders.len())),
        );
    }

    header.into_any_element()
}

/// 空状态：图标 + 「还没有音乐哦」+ 说明 + 「添加文件夹」按钮。
fn empty_state(theme: &UiTheme) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.))
        // 视觉重心略微上移，避免被底部浮动栏压得偏下。
        .pb(px(BOTTOM_CLEARANCE))
        .child(
            div()
                .size(px(64.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.colors.muted)
                .child(
                    lucide(LucideIcon::Music)
                        .size(px(28.))
                        .text_color(theme.colors.muted_foreground),
                ),
        )
        .child(
            div()
                .mt(px(6.))
                .text_size(px(18.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("还没有音乐哦"),
        )
        .child(
            div()
                .max_w(px(360.))
                .text_center()
                .text_size(theme.text(13.))
                .text_color(theme.colors.muted_foreground)
                .child("添加一个存放歌曲的文件夹，里面的音乐会被整理进音乐库。"),
        )
        .child(div().mt(px(10.)).child(add_folder_button(theme)))
        .into_any_element()
}

/// 主操作按钮：选文件夹并入音乐库。
fn add_folder_button(theme: &UiTheme) -> Button {
    let content = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(
            lucide(LucideIcon::FolderPlus)
                .size(px(15.))
                .text_color(theme.colors.primary_foreground),
        )
        .child(
            div()
                .text_size(theme.text(13.))
                .font_weight(FontWeight::MEDIUM)
                .child("添加文件夹"),
        );

    Button::new("library-add-folder")
        .variant(ButtonVariant::Default)
        .size(ButtonSize::Default)
        .aria_label("添加文件夹")
        .child(content)
        .on_click(|_, _, cx| pick_folders(cx))
}

/// 已有文件夹时的主体：左栏文件夹切换 + 右栏歌曲列表。
fn library_body(
    state: &LibraryState,
    theme: &UiTheme,
    now: &audio::ListMark,
    cx: &mut App,
) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .px(px(PAGE_PADDING_X))
        .pb(px(BOTTOM_CLEARANCE))
        .flex()
        .gap(px(18.))
        .child(folder_pane(state, theme))
        .child(track_pane(state, theme, now, cx))
        .into_any_element()
}

/// 左栏：文件夹列表，顶部是「文件夹」标题 + 添加入口。
fn folder_pane(state: &LibraryState, theme: &UiTheme) -> AnyElement {
    let rows: Vec<AnyElement> = state
        .folders
        .iter()
        .enumerate()
        .map(|(index, path)| folder_row(index, path, state.selected, theme))
        .collect();

    div()
        .w(px(FOLDER_PANE_WIDTH))
        .flex_shrink_0()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .flex_shrink_0()
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(theme.text(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.colors.muted_foreground)
                        .child("文件夹"),
                )
                .child(add_folder_icon_button(theme)),
        )
        .child(
            div()
                .id("library-folder-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(rows),
        )
        .into_any_element()
}

/// 单个文件夹条目：图标 + 名称，选中项用浅底高亮。
fn folder_row(index: usize, path: &Path, selected: usize, theme: &UiTheme) -> AnyElement {
    let is_selected = index == selected;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    div()
        .id(("library-folder", index))
        .h(px(FOLDER_ROW_HEIGHT))
        .flex_shrink_0()
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(theme.radius.sm)
        .cursor_pointer()
        .text_color(if is_selected {
            theme.colors.foreground
        } else {
            theme.colors.muted_foreground
        })
        .when(is_selected, |el| el.bg(theme.colors.muted))
        .when(!is_selected, |el| {
            el.hover(|style| {
                style
                    .bg(theme.colors.muted)
                    .text_color(theme.colors.foreground)
            })
        })
        .child(
            lucide(if is_selected {
                LucideIcon::FolderOpen
            } else {
                LucideIcon::Folder
            })
            .size(px(15.)),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(theme.text(13.))
                .child(name),
        )
        .on_click(move |_: &ClickEvent, _: &mut Window, cx: &mut App| select_folder(cx, index))
        .into_any_element()
}

/// 左栏标题行右侧的小图标按钮：添加文件夹。
fn add_folder_icon_button(theme: &UiTheme) -> AnyElement {
    div()
        .id("library-add-folder-mini")
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius.sm)
        .cursor_pointer()
        .hover(|style| style.bg(theme.colors.muted))
        .child(
            lucide(LucideIcon::FolderPlus)
                .size(px(14.))
                .text_color(theme.colors.muted_foreground),
        )
        .on_click(|_: &ClickEvent, _: &mut Window, cx: &mut App| pick_folders(cx))
        .into_any_element()
}

/// 右栏：标题（文件夹名 + 数量）+ 歌曲列表。
fn track_pane(
    state: &LibraryState,
    theme: &UiTheme,
    now: &audio::ListMark,
    cx: &mut App,
) -> AnyElement {
    let folder_name = state
        .current()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "音乐库".to_string());

    let mut count = format!("{} 首", state.files.len());
    if state.scanning {
        count.push_str(" · 扫描中…");
    }

    let body = if state.files.is_empty() {
        let hint = if state.scanning {
            "正在扫描这个文件夹…"
        } else {
            "这个文件夹里还没有识别到音乐（mp3 / wav / flac / m4a 等）"
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .text_size(theme.text(13.))
            .text_color(theme.colors.muted_foreground)
            .child(hint)
            .into_any_element()
    } else {
        let rows: Vec<AnyElement> = state
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                track_row(index, file, state.menus.get(index), theme, now, cx)
            })
            .collect();
        div()
            .id("library-track-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            // 卡片之间有缝，圆角边框才不会连成一条。
            .gap(px(6.))
            .pb(px(6.))
            .children(rows)
            .into_any_element()
    };

    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_shrink_0()
                .h(px(28.))
                .pb(px(6.))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(theme.text(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(folder_name),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .pl(px(12.))
                        .text_size(theme.text(11.))
                        .text_color(theme.colors.muted_foreground)
                        .child(count),
                ),
        )
        .child(track_header(theme))
        .child(body)
        .into_any_element()
}

/// 列表表头：`# / 标题 / 专辑 / 操作 / 时长 / 大小`，列宽与卡片内一致。
fn track_header(theme: &UiTheme) -> AnyElement {
    let label = |text: &'static str| {
        div()
            .text_size(px(11.))
            .text_color(theme.colors.muted_foreground)
            .child(text)
    };

    div()
        .flex_shrink_0()
        .h(px(TRACK_HEADER_HEIGHT))
        .px(px(TRACK_CARD_PADDING))
        .flex()
        .items_center()
        .gap(px(TRACK_COLUMN_GAP))
        .child(
            div()
                .w(px(TRACK_PLAY_SIZE))
                .flex_shrink_0()
                .child(label("#")),
        )
        .child(
            div()
                .w(px(TRACK_COVER_SIZE))
                .flex_shrink_0()
                .child(label("标题")),
        )
        .child(div().flex_1().min_w_0())
        .child(div().w(px(COL_ALBUM)).flex_shrink_0().child(label("专辑")))
        .child(
            div()
                .w(px(COL_ACTION))
                .flex_shrink_0()
                .text_center()
                .child(label("操作")),
        )
        .child(
            div()
                .w(px(COL_DURATION))
                .flex_shrink_0()
                .text_right()
                .child(label("时长")),
        )
        .child(
            div()
                .w(px(COL_SIZE))
                .flex_shrink_0()
                .text_right()
                .child(label("大小")),
        )
        .into_any_element()
}

/// 单首歌曲：卡片形态（播放按钮 + 封面 + 标题/艺人 + 专辑 + 收藏 + 时长 + 大小），
/// 整张卡片是一个右键菜单区域。
fn track_row(
    index: usize,
    file: &AudioFile,
    menu: Option<&Entity<MenuState>>,
    theme: &UiTheme,
    now: &audio::ListMark,
    cx: &mut App,
) -> AnyElement {
    let artist = file
        .artist
        .clone()
        .unwrap_or_else(|| "未知艺术家".to_string());
    let badge = file.extension.to_uppercase();
    // 这一首是不是正在播 / 暂停的那首：按钮图形与卡片边框跟着变。
    let is_current = now.path.as_deref() == Some(file.path.as_path());
    let is_playing = is_current && now.playing;
    // 封面缩略图：缓存里有就画图，没有先画音符占位 —— `thumb` 会顺手把这首
    // 排进后台队列，读回来重绘一次就换成真图。
    let cover = match covers::thumb(cx, &file.path) {
        Some(source) => div()
            .size(px(TRACK_COVER_SIZE))
            .flex_shrink_0()
            .rounded(px(TRACK_COVER_RADIUS))
            .overflow_hidden()
            .child(
                primitives::image(source)
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            ),
        None => div()
            .size(px(TRACK_COVER_SIZE))
            .flex_shrink_0()
            .rounded(px(TRACK_COVER_RADIUS))
            .bg(theme.colors.muted)
            .flex()
            .items_center()
            .justify_center()
            .child(
                lucide(LucideIcon::Music)
                    .size(px(18.))
                    .text_color(theme.colors.muted_foreground),
            ),
    };
    let card = div()
        .id(("library-track", index))
        .h(px(TRACK_ROW_HEIGHT))
        .flex_shrink_0()
        .px(px(TRACK_CARD_PADDING))
        .flex()
        .items_center()
        .gap(px(TRACK_COLUMN_GAP))
        .rounded(px(TRACK_CARD_RADIUS))
        .bg(theme.colors.sidebar)
        .border_1()
        .border_color(theme.colors.border)
        .hover(|style| style.bg(theme.colors.muted))
        // 正在播的那首用高亮边框标出来。
        .when(is_current, |el| el.border_color(theme.colors.foreground))
        // 圆形播放按钮：正在播放时显示暂停图标。
        .child(
            div()
                .id(("library-track-play", index))
                .size(px(TRACK_PLAY_SIZE))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(theme.colors.border)
                .cursor_pointer()
                .hover(|style| {
                    style
                        .bg(theme.colors.accent)
                        .border_color(theme.colors.muted_foreground)
                })
                .child(
                    lucide(if is_playing {
                        LucideIcon::Pause
                    } else {
                        LucideIcon::Play
                    })
                    .size(px(12.))
                    .text_color(theme.colors.foreground),
                )
                .on_click(move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    play_from_library(cx, index)
                }),
        )
        .child(cover)
        // 标题 + 格式徽章 + 艺人
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .truncate()
                        .text_size(theme.text(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(file.title.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex_shrink_0()
                                .px(px(4.))
                                .py(px(1.))
                                .rounded(px(3.))
                                .bg(theme.colors.muted)
                                .text_size(px(9.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.colors.muted_foreground)
                                .child(badge),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(theme.text(11.))
                                .text_color(theme.colors.muted_foreground)
                                .child(artist),
                        ),
                ),
        )
        // 专辑：标签里没有就显示「未知专辑」
        .child(
            div()
                .w(px(COL_ALBUM))
                .flex_shrink_0()
                .truncate()
                .text_size(theme.text(11.))
                .text_color(theme.colors.muted_foreground)
                .child(file.album.clone().unwrap_or_else(|| "未知专辑".to_string())),
        )
        // 收藏
        .child(
            div()
                .id(("library-track-like", index))
                .w(px(COL_ACTION))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|style| style.text_color(theme.colors.foreground))
                .child(
                    lucide(LucideIcon::Heart)
                        .size(px(16.))
                        .text_color(theme.colors.muted_foreground),
                ),
        )
        // 时长（还没解析，先占位）
        .child(
            div()
                .w(px(COL_DURATION))
                .flex_shrink_0()
                .text_right()
                .text_size(theme.text(11.))
                .text_color(theme.colors.muted_foreground)
                .child("--:--"),
        )
        // 文件大小
        .child(
            div()
                .w(px(COL_SIZE))
                .flex_shrink_0()
                .text_right()
                .text_size(theme.text(11.))
                .text_color(theme.colors.muted_foreground)
                .child(size_label(file.size)),
        );

    match menu {
        Some(menu) => Menu::new(menu, "曲目操作")
            .context_area(card)
            .into_any_element(),
        None => card.into_any_element(),
    }
}

/// 单曲卡片的右键菜单项：播放、下一首播放 / 修改歌词、编辑元数据、打开文件位置、删除文件。
fn track_menu_items(index: usize, file: &AudioFile) -> Vec<MenuItem> {
    let reveal = file.path.clone();
    let target = file.clone();
    let queued = track_from(file);
    // 「修改歌词」要把这首歌带进编辑器的独立窗口，编辑器才知道歌词写回哪个文件。
    let lyric_song = crate::pages::lyricseditor::Song {
        path: file.path.clone(),
        title: file.title.clone(),
        artist: file.artist.clone(),
    };
    vec![
        MenuItem::new("track-play", "播放")
            .icon(LucideIcon::Play)
            .on_click(move |_, _, cx| play_from_library(cx, index)),
        MenuItem::new("track-play-next", "下一首播放")
            .icon(LucideIcon::ListPlus)
            .on_click(move |_, _, cx| audio::insert_next(cx, queued.clone())),
        MenuItem::separator(),
        MenuItem::new("track-edit-lyrics", "修改歌词")
            .icon(LucideIcon::FileText)
            .on_click(move |_, _, cx| {
                crate::pages::lyricseditor::window::open_for_song(cx, lyric_song.clone())
            }),
        MenuItem::new("track-edit-meta", "编辑元数据")
            .icon(LucideIcon::Tags)
            .on_click(move |_, window, cx| super::metadata::open(window, cx, &target)),
        MenuItem::new("track-reveal", "打开文件位置")
            .icon(LucideIcon::FolderOpen)
            .on_click(move |_, _, _| reveal_in_file_manager(&reveal)),
        MenuItem::new("track-delete", "删除文件")
            .icon(LucideIcon::Trash)
            .danger(true)
            // 删除涉及真实文件，先不接实现，避免误删。
            .on_click(|_, _, _| {}),
    ]
}

/// 在系统文件管理器里选中这个文件。
fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = path.parent().unwrap_or(path);
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

// ---- 交互 ----

/// 把音乐库条目转成播放队列里的一项。
fn track_from(file: &AudioFile) -> Track {
    let mut track = Track::new(file.path.clone(), file.title.clone());
    if let Some(artist) = &file.artist {
        track = track.with_artist(artist.clone());
    }
    track
}

/// 点某首的播放按钮：正在放这首就暂停 / 继续，否则整个文件夹入队、从这首开播。
fn play_from_library(cx: &mut App, index: usize) {
    let state = state(cx);
    let Some(file) = state.files.get(index) else {
        return;
    };
    // 同一首：按暂停 / 继续处理，不要从头重放。
    let now = audio::state(cx);
    if now
        .current()
        .map(|track| track.path == file.path)
        .unwrap_or(false)
    {
        audio::toggle(cx);
        return;
    }
    // 队列就是当前文件夹的全部曲目，左右切歌才有意义。
    let queue: Vec<Track> = state.files.iter().map(track_from).collect();
    audio::play(cx, queue, index);
}

/// 弹系统目录对话框，把选中的文件夹并入音乐库，并切到新加的那个。
fn pick_folders(cx: &mut App) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: true,
        prompt: Some("选择音乐文件夹".into()),
    });

    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = receiver.await else {
            return;
        };
        let mut index = None;
        cx.update(|cx| {
            update(cx, |state| {
                for path in paths {
                    if !state.folders.contains(&path) {
                        state.folders.push(path);
                    }
                }
                // 切到新加的那个（去重后可能没变，取末尾兜底）。
                state.selected = state.folders.len().saturating_sub(1);
                index = Some(state.selected);
            });
        });
        if let Some(index) = index {
            cx.update(|cx| scan(cx, index));
        }
    })
    .detach();
}

/// 左栏点选文件夹：切换选中并重新扫描。
fn select_folder(cx: &mut App, index: usize) {
    let mut changed = false;
    update(cx, |state| {
        if state.selected != index && index < state.folders.len() {
            state.selected = index;
            changed = true;
        }
    });
    if changed {
        scan(cx, index);
    }
}

/// 扫描指定文件夹：先清空并标记扫描中，再在后台线程遍历目录。
fn scan(cx: &mut App, index: usize) {
    let Some(folder) = state(cx).folders.get(index).cloned() else {
        return;
    };

    let mut generation = 0;
    update(cx, |state| {
        state.generation += 1;
        state.files.clear();
        state.menus.clear();
        state.scanning = true;
        generation = state.generation;
    });
    // 页面是无状态函数，改完全局状态要主动刷新窗口。
    cx.refresh_windows();

    let task = cx
        .background_executor()
        .spawn(async move { scan_folder(&folder) });

    cx.spawn(async move |cx| {
        let files = task.await;
        cx.update(|cx| {
            let mut next = state(cx);
            // 期间又换了文件夹的话，这份结果已经过期了。
            if next.generation != generation {
                return;
            }
            next.scanning = false;
            next.files = files;
            // 每张卡片一个独立的菜单状态，避免多个 context_area 共用同一个弹层。
            let menus = next
                .files
                .iter()
                .enumerate()
                .map(|(index, file)| {
                    let items = track_menu_items(index, file);
                    cx.new(|cx| MenuState::new(items, cx))
                })
                .collect();
            next.menus = menus;
            cx.set_global(next);
            cx.refresh_windows();
        });
    })
    .detach();
}

// ---- 扫描 ----

/// 递归收集一个文件夹下的音频文件，按标题排序。
fn scan_folder(root: &Path) -> Vec<AudioFile> {
    let mut files = Vec::new();
    collect(root, 0, &mut files);
    files.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    files
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<AudioFile>) {
    if depth > MAX_DEPTH || out.len() >= MAX_TRACKS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect(&path, depth + 1, out);
        } else if file_type.is_file() {
            if let Some(file) = audio_file(&path) {
                out.push(file);
                if out.len() >= MAX_TRACKS {
                    return;
                }
            }
        }
    }
}

/// 把路径识别成音频文件；不认识的扩展名返回 `None`。
fn audio_file(path: &Path) -> Option<AudioFile> {
    let extension = path.extension()?.to_string_lossy().to_lowercase();
    if !AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        return None;
    }

    let stem = path.file_stem()?.to_string_lossy().to_string();
    // 常见命名「艺人 - 曲名」，拆成两段；否则整段当标题。
    let (artist, title) = match stem.split_once(" - ") {
        Some((artist, title)) if !artist.trim().is_empty() && !title.trim().is_empty() => {
            (Some(artist.trim().to_string()), title.trim().to_string())
        }
        _ => (None, stem),
    };

    // 文件里写了标签就按标签走 —— 这样改过的元数据重启后还在。
    let (title, artist, album) = match crate::services::tags::read(path) {
        Ok(tags) => (
            trimmed_or(tags.title, title),
            trimmed_option(tags.artist).or(artist),
            trimmed_option(tags.album),
        ),
        // 标签读不出来（坏文件、不支持的格式）就继续用文件名推断的结果。
        Err(_) => (title, artist, None),
    };

    let size = path.metadata().map(|meta| meta.len()).unwrap_or(0);

    Some(AudioFile {
        path: path.to_path_buf(),
        title,
        artist,
        album,
        extension,
        size,
    })
}

/// 标签值非空就用它，否则退回文件名推断的值。
fn trimmed_or(value: String, fallback: String) -> String {
    if value.trim().is_empty() {
        fallback
    } else {
        value.trim().to_string()
    }
}

/// 标签值整理成 `Option`，空字符串当成没有。
fn trimmed_option(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// 字节数转成 `4.2 MB` / `860 KB` 这种展示文本。
fn size_label(bytes: u64) -> String {
    const MB: f64 = 1024. * 1024.;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", (bytes as f64 / 1024.).round().max(1.) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端：点某首的播放按钮 → 队列进引擎 → 真的起播 → 再点一次变暂停。
    ///
    /// 用系统自带的 aiff 当素材，省得造文件；没有音频设备的环境直接跳过。
    #[gpui_kit::test]
    fn play_button_starts_and_toggles_playback(cx: &mut gpui_kit::TestAppContext) {
        let sample = Path::new("/System/Library/Sounds/Ping.aiff");
        if !sample.exists() {
            return;
        }

        cx.update(|cx| {
            crate::audio::init(cx);
            let file = audio_file(sample).expect("应识别为音频");
            update(cx, |state| {
                state.folders = vec![sample.parent().unwrap().to_path_buf()];
                state.selected = 0;
                state.files = vec![file];
            });

            play_from_library(cx, 0);
            let now = crate::audio::state(cx);
            assert!(now.current().is_some(), "队列里应有当前曲目");
            assert!(
                now.playing,
                "点了播放就该开始出声，实际报错：{:?}",
                now.error
            );
            assert!(now.duration > 0., "起播后应读到时长");

            // 刚起播时计时器不该把「还没出声」误判成「已播完」而跳歌。
            for _ in 0..3 {
                crate::audio::tick(cx);
            }
            let ticking = crate::audio::state(cx);
            assert_eq!(ticking.index, 0, "计时器不该跳曲目");
            assert!(ticking.playing, "计时器不该把播放状态改掉");

            // 同一首再点一次：变成暂停，不要从头重播。
            play_from_library(cx, 0);
            let paused = crate::audio::state(cx);
            assert!(!paused.playing, "再点一次应该暂停");
            assert_eq!(paused.index, 0, "暂停不该换曲目");
        });
    }

    #[test]
    fn recognizes_audio_extensions_case_insensitively() {
        let file = audio_file(Path::new("/music/Artist - Title.MP3")).expect("应识别为音频");
        assert_eq!(file.extension, "mp3");
        assert_eq!(file.artist.as_deref(), Some("Artist"));
        assert_eq!(file.title, "Title");
    }

    #[test]
    fn ignores_non_audio_files() {
        assert!(audio_file(Path::new("/music/cover.jpg")).is_none());
        assert!(audio_file(Path::new("/music/notes.txt")).is_none());
    }

    #[test]
    fn keeps_plain_stem_as_title() {
        let file = audio_file(Path::new("/music/夜航.wav")).expect("应识别为音频");
        assert_eq!(file.title, "夜航");
        assert!(file.artist.is_none());
    }

    #[test]
    fn sizes_switch_to_megabytes() {
        assert_eq!(size_label(512), "1 KB");
        assert_eq!(size_label(1024 * 900), "900 KB");
        assert_eq!(size_label(1024 * 1024 * 5), "5.0 MB");
    }

    /// 在 `target/` 下造一棵小目录树，验证递归扫描、扩展名过滤与排序。
    #[test]
    fn scans_subfolders_and_skips_other_files() {
        let root = std::env::current_dir()
            .expect("取工作目录")
            .join("target")
            .join(format!("scan-fixture-{}", std::process::id()));
        let album = root.join("album");
        std::fs::create_dir_all(&album).expect("建测试目录");
        std::fs::write(root.join("b.wav"), b"12345").expect("写测试文件");
        std::fs::write(root.join("cover.jpg"), b"x").expect("写非音频文件");
        std::fs::write(album.join("a.mp3"), b"123").expect("写子目录音频");

        let files = scan_folder(&root);
        let titles: Vec<&str> = files.iter().map(|file| file.title.as_str()).collect();
        assert_eq!(titles, ["a", "b"]);
        assert_eq!(files[0].extension, "mp3");
        assert_eq!(files[1].size, 5);
    }
}
