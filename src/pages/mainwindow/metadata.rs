//! 元数据编辑弹窗。
//!
//! 音乐库卡片的右键菜单「编辑元数据」打开它：8 个单行输入框
//! （标题 / 艺术家 / 专辑 / 专辑艺术家 / 年份 / 流派 / 音轨号 / 碟号）、
//! 一行封面预览、两张动作卡片（上传封面 / 匹配元数据），右下角是「保存」和「取消」。
//!
//! 打开时先读文件里已有的标签和封面回填（读不到就退回音乐库从文件名推断的值）；
//! 点「保存」时把 8 个字段和封面一起写进音频文件（ID3v2 / Vorbis / MP4 ilst 等），
//! 同时更新内存里的音乐库条目，并落盘音源匹配到的歌词。

use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use crate::{
    pages::mainwindow::library::{self, AudioFile},
    services::{lx, tags},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        dialog::{DialogHandle, dialog_footer, dialog_popup, dialog_title, dialog_with},
        input::{Input, InputState},
        theme::{UiTheme, primitives},
    },
};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, FontWeight, Global, Image, ImageFormat, ImageSource,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, PathPromptOptions,
    StatefulInteractiveElement as _, Styled, StyledImage as _, Window, div, px,
};

/// 封面缩略图的边长。
const COVER_TILE_SIZE: f32 = 48.;
/// 内嵌封面的大小上限：标签里塞超大图会让音频文件凭空翻倍，读封面也变慢。
const MAX_COVER_BYTES: usize = 8 * 1024 * 1024;

/// 表单字段：(标签, 占位文本)。顺序即 4 行 × 2 列的填充顺序。
const FIELDS: [(&str, &str); 8] = [
    ("标题", "歌曲标题"),
    ("艺术家", "歌手"),
    ("专辑", "专辑名"),
    ("专辑艺术家", "专辑艺术家"),
    ("年份", "如 2024"),
    ("流派", "如 流行"),
    ("音轨号", "如 1"),
    ("碟号", "如 1"),
];

/// 表单下方的两张动作卡片：(id, 粗体标题, 灰色说明)。
const ACTION_CARDS: [(&str, &str, &str); 2] = [
    ("metadata-upload-cover", "上传封面", "点我自定义歌曲的封面"),
    (
        "metadata-match",
        "匹配元数据",
        "填写一下歌曲标题，点我然后从音源获取歌曲元数据和歌词",
    ),
];

/// 弹窗状态：句柄、正在编辑的文件、8 个输入框、封面、匹配进度、待落盘的歌词。
pub struct MetadataEditor {
    handle: DialogHandle,
    file: Rc<RefCell<Option<PathBuf>>>,
    fields: Vec<Entity<InputState>>,
    status: Rc<RefCell<Option<Status>>>,
    /// 音源拿回来的歌词：点「保存」时才写进 `.lrc`，取消就丢掉。
    pending_lyric: Rc<RefCell<Option<String>>>,
    /// 弹窗上当前这张封面（打开时从文件读，选图 / 移除后替换）。
    cover: Rc<RefCell<Option<CoverView>>>,
    /// 用户是否动过封面：没动过就原样保留，不把文件里原有的图重写一遍。
    cover_dirty: Rc<Cell<bool>>,
}

/// 卡片下方那行提示文字。
#[derive(Clone, Debug)]
struct Status {
    text: String,
    error: bool,
}

impl Status {
    fn busy(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    fn done(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }
}

/// 弹窗里正在展示的封面：既可能是文件里原有的，也可能是刚选好的。
struct CoverView {
    /// 要写进文件的内容（字节 + MIME）。
    cover: tags::Cover,
    /// 展示用的来源名：选文件时是文件名，从文件里读出来的是「文件内嵌封面」。
    name: String,
    /// 预先算好的图片源。每帧从字节重建会把整张图复制一遍，所以只建这一次。
    image: Option<ImageSource>,
}

impl CoverView {
    fn new(cover: tags::Cover, name: String) -> Self {
        // 容器里存的 MIME 可能不是 gpui 认识的（比如旧文件写了 image/webp），
        // 那就只显示图标，至少不挡住别的操作。
        let image = ImageFormat::from_mime_type(&cover.mime).map(|format| {
            ImageSource::from(Arc::new(Image::from_bytes(format, cover.bytes.clone())))
        });
        Self {
            cover,
            name,
            image,
        }
    }

    /// 给用户看的「名字 · 大小」。
    fn summary(&self) -> String {
        format!("{} · {}", self.name, human_size(self.cover.bytes.len()))
    }
}

/// 文件大小的口语写法（按 1024 进制，跟 Finder 一致）。
fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.;
    const MB: f64 = KB * 1024.;
    let bytes = bytes as f64;
    if bytes >= MB {
        format!("{:.1} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{} B", bytes as usize)
    }
}

impl Global for MetadataEditor {}

/// 启动时建好输入框与句柄，挂进全局。
pub fn init(window: &mut Window, cx: &mut App) {
    let fields = FIELDS
        .iter()
        .map(|(_, placeholder)| cx.new(|cx| InputState::new(window, cx).placeholder(*placeholder)))
        .collect();

    cx.set_global(MetadataEditor {
        handle: DialogHandle::new(false),
        file: Rc::new(RefCell::new(None)),
        fields,
        status: Rc::new(RefCell::new(None)),
        pending_lyric: Rc::new(RefCell::new(None)),
        cover: Rc::new(RefCell::new(None)),
        cover_dirty: Rc::new(Cell::new(false)),
    });
}

/// 用某个文件的现有信息填充表单并打开弹窗。
pub fn open(window: &mut Window, cx: &mut App, file: &AudioFile) {
    // 先把需要的句柄克隆出来，避免全局借用与 `cx` 的可变借用冲突。
    let Some((handle, file_slot, fields, cover, cover_dirty, status, pending)) = cx
        .try_global::<MetadataEditor>()
        .map(|editor| {
            (
                editor.handle.clone(),
                editor.file.clone(),
                editor.fields.clone(),
                editor.cover.clone(),
                editor.cover_dirty.clone(),
                editor.status.clone(),
                editor.pending_lyric.clone(),
            )
        })
    else {
        return;
    };

    // 顺序与 `FIELDS` 一致：文件里的标签优先，读不到再退回音乐库从文件名推断的值。
    let stored = tags::read(&file.path).unwrap_or_default();
    let values = [
        pick(stored.title, file.title.clone()),
        pick(stored.artist, file.artist.clone().unwrap_or_default()),
        stored.album,
        stored.album_artist,
        stored.year,
        stored.genre,
        stored.track_no,
        stored.disc_no,
    ];
    for (index, field) in fields.iter().enumerate() {
        let value = values.get(index).cloned().unwrap_or_default();
        field.update(cx, |state, cx| state.set_value(value, window, cx));
    }

    *file_slot.borrow_mut() = Some(file.path.clone());
    // 换了一首歌：上一首的封面、匹配结果和提示都要清掉。
    *cover.borrow_mut() =
        tags::read_cover(&file.path).map(|cover| CoverView::new(cover, "文件内嵌封面".into()));
    cover_dirty.set(false);
    *status.borrow_mut() = None;
    *pending.borrow_mut() = None;
    handle.open(window, cx);
}

/// 渲染弹窗（挂在主窗口最外层）。未初始化时返回空元素。
pub fn dialog(window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(editor) = cx.try_global::<MetadataEditor>() else {
        return div().into_any_element();
    };

    let handle = editor.handle.clone();
    let fields = editor.fields.clone();
    let status = editor.status.clone();
    let cover = editor.cover.clone();
    let cover_dirty = editor.cover_dirty.clone();
    let popup_handle = handle.clone();

    dialog_with(
        "metadata-dialog",
        &handle,
        move |_window, cx| popup(&fields, &status, &cover, &cover_dirty, &popup_handle, cx),
        window,
        cx,
    )
    .into_any_element()
}

/// 弹窗内容：标题 + 4 行两列表单 + 封面预览 + 两张动作卡片 + 右下角操作按钮。
fn popup(
    fields: &[Entity<InputState>],
    status: &Rc<RefCell<Option<Status>>>,
    cover: &Rc<RefCell<Option<CoverView>>>,
    cover_dirty: &Rc<Cell<bool>>,
    handle: &DialogHandle,
    cx: &mut App,
) -> AnyElement {
    let theme = UiTheme::read(cx).clone();

    let mut grid = div().flex().flex_col().gap(px(12.));
    for row in 0..4 {
        let mut line = div().flex().gap(px(12.));
        for column in 0..2 {
            let index = row * 2 + column;
            let (label, _) = FIELDS[index];
            line = line.child(field(&fields[index], label, &theme));
        }
        grid = grid.child(line);
    }

    // 表单与卡片之间：一张缩略图 + 一句话说明「现在是什么、保存会发生什么」。
    let cover_row = {
        let current = cover.borrow();
        cover_row(current.as_ref(), cover_dirty.get(), &theme)
    };

    // 表单下方：两张竖向排布的动作卡片，每张两行左对齐文本。
    let mut actions = div().flex().flex_col().gap(px(10.));
    for (id, title, description) in ACTION_CARDS {
        actions = actions.child(action_card(id, title, description, &theme));
    }
    // 「匹配元数据」的结果提示跟在卡片下面。
    if let Some(status) = status.borrow().clone() {
        actions = actions.child(status_row(&status, &theme));
    }

    let cancel_handle = handle.clone();
    let save_handle = handle.clone();

    dialog_popup("metadata-popup", "编辑元数据", cx)
        .max_w(px(560.))
        .child(dialog_title("metadata-title", cx).child("编辑元数据"))
        .child(grid)
        .child(cover_row)
        .child(actions)
        .child(
            dialog_footer(cx)
                .child(
                    Button::new("metadata-cancel")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .child("取消")
                        .on_click(move |_, window, cx| cancel_handle.close(window, cx)),
                )
                .child(
                    Button::new("metadata-save")
                        .variant(ButtonVariant::Default)
                        .size(ButtonSize::Sm)
                        .child("保存")
                        .on_click(move |_, window, cx| {
                            // 写文件失败时弹窗留着，让用户看到失败原因，别把输入一起吞了。
                            if save(window, cx) {
                                save_handle.close(window, cx);
                            }
                        }),
                ),
        )
        .into_any_element()
}

/// 封面预览行：缩略图 + 「现在是什么 / 保存会发生什么」+ 移除按钮。
fn cover_row(cover: Option<&CoverView>, dirty: bool, theme: &UiTheme) -> AnyElement {
    let (title, detail) = match (cover, dirty) {
        (Some(view), true) => (
            "已选择封面".to_string(),
            format!("{} · 保存后写进歌曲", view.summary()),
        ),
        (Some(view), false) => (
            "当前封面".to_string(),
            format!("{} · 文件里已有这张", view.summary()),
        ),
        (None, true) => (
            "还没有封面".to_string(),
            "保存后会把文件里原来的封面删掉".to_string(),
        ),
        (None, false) => (
            "还没有封面".to_string(),
            "点下面的「上传封面」选一张图片".to_string(),
        ),
    };

    let text = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .truncate()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.colors.foreground)
                .child(title),
        )
        .child(
            div()
                .truncate()
                .text_size(px(12.))
                .text_color(theme.colors.muted_foreground)
                .child(detail),
        );

    // 有封面 / 没封面用两个 id，测试据此判断该显示哪一种状态。
    let (id, selector) = if cover.is_some() {
        ("metadata-cover-filled", "metadata-cover-filled")
    } else {
        ("metadata-cover-empty", "metadata-cover-empty")
    };

    let mut row = div()
        .id(id)
        .debug_selector(move || selector.into())
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(12.))
        .child(cover_tile(cover, theme))
        .child(text);

    if cover.is_some() {
        row = row.child(
            // 包一层带 id 的容器：按钮自己的 id 不一定进调试选择器，
            // 测试要能定位到「移除」这块热区。
            div()
                .id("metadata-cover-remove")
                .debug_selector(|| "metadata-cover-remove".into())
                .child(
                    Button::new("metadata-cover-remove-button")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Sm)
                        .child("移除")
                        .on_click(|_, _, cx| remove_cover(cx)),
                ),
        );
    }

    row.into_any_element()
}

/// 封面缩略图：有图就画图，没有就画一个占位图标。
fn cover_tile(cover: Option<&CoverView>, theme: &UiTheme) -> AnyElement {
    let frame = div()
        .size(px(COVER_TILE_SIZE))
        .flex_shrink_0()
        .rounded(px(10.))
        .overflow_hidden()
        .border_1()
        .border_color(theme.colors.border)
        .bg(theme.colors.muted);

    match cover.and_then(|view| view.image.clone()) {
        Some(source) => frame
            .child(
                primitives::image(source)
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            )
            .into_any_element(),
        None => frame
            .flex()
            .items_center()
            .justify_center()
            .child(
                lucide(LucideIcon::Image)
                    .size(px(18.))
                    .text_color(theme.colors.muted_foreground),
            )
            .into_any_element(),
    }
}

/// 匹配结果提示：普通状态用灰字，失败用警示色。
fn status_row(status: &Status, theme: &UiTheme) -> AnyElement {
    let color = if status.error {
        theme.colors.destructive
    } else {
        theme.colors.muted_foreground
    };

    div()
        .w_full()
        .min_w_0()
        .px(px(2.))
        .text_size(px(12.))
        .text_color(color)
        .child(status.text.clone())
        .into_any_element()
}

/// 单个字段：小标签 + 单行输入框。
fn field(input: &Entity<InputState>, label: &str, theme: &UiTheme) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.colors.muted_foreground)
                .child(label.to_string()),
        )
        .child(Input::new(input).w_full().aria_label(label.to_string()))
        .into_any_element()
}

/// 一张动作卡片：粗体标题 + 灰色小字说明，整卡可点、悬停有底色反馈。
fn action_card(
    id: &'static str,
    title: &'static str,
    description: &'static str,
    theme: &UiTheme,
) -> AnyElement {
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(14.))
        .py(px(12.))
        .rounded(px(12.))
        .border_1()
        .border_color(theme.colors.border)
        .bg(theme.colors.card)
        .cursor_pointer()
        .hover(|style| style.bg(theme.colors.muted))
        .on_click(move |_, _, cx| match id {
            "metadata-upload-cover" => pick_cover(cx),
            "metadata-match" => start_match(cx),
            _ => {}
        })
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.colors.foreground)
                .child(title),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.colors.muted_foreground)
                .child(description),
        )
        .into_any_element()
}

/// 点「上传封面」：弹系统文件对话框选一张图，选完交给 [`start_cover_load`]。
fn pick_cover(cx: &mut App) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("选择封面图片".into()),
    });

    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = receiver.await else {
            return;
        };
        let Some(path) = paths.into_iter().next() else {
            return;
        };
        cx.update(|cx| start_cover_load(cx, path));
    })
    .detach();
}

/// 读图可能有几 MB，扔后台；读完回到 [`apply_cover`] 填预览。
fn start_cover_load(cx: &mut App, path: PathBuf) {
    let Some(status) = cx
        .try_global::<MetadataEditor>()
        .map(|editor| editor.status.clone())
    else {
        return;
    };
    let name = file_label(&path);
    *status.borrow_mut() = Some(Status::busy(format!("正在读取 {name}…")));
    cx.refresh_windows();

    let task = cx
        .background_executor()
        .spawn(async move { read_cover_file(&path) });

    cx.spawn(async move |cx| {
        let result = task.await.map(|cover| CoverView::new(cover, name));
        cx.update(|cx| apply_cover(cx, result));
    })
    .detach();
}

/// 读图片字节并认格式，MIME 按内容判断（扩展名不可信）。
fn read_cover_file(path: &Path) -> Result<tags::Cover, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("读不出这张图：{error}"))?;
    if bytes.len() > MAX_COVER_BYTES {
        return Err(format!(
            "这张图有 {}，超过 {} 的上限，换张小一点的",
            human_size(bytes.len()),
            human_size(MAX_COVER_BYTES)
        ));
    }
    let mime = tags::sniff_cover_mime(&bytes).ok_or_else(|| {
        format!(
            "{} 不是支持的图片格式，用 PNG / JPEG / GIF / BMP / TIFF",
            file_label(path)
        )
    })?;

    Ok(tags::Cover {
        bytes,
        mime: mime.to_string(),
    })
}

/// 选好图之后：换掉预览，标记「保存时要写封面」。
fn apply_cover(cx: &mut App, result: Result<CoverView, String>) {
    let Some((cover, dirty, status)) = cx.try_global::<MetadataEditor>().map(|editor| {
        (
            editor.cover.clone(),
            editor.cover_dirty.clone(),
            editor.status.clone(),
        )
    }) else {
        return;
    };

    match result {
        Ok(view) => {
            let note = format!("已选择 {}，保存后写进歌曲", view.summary());
            *cover.borrow_mut() = Some(view);
            dirty.set(true);
            *status.borrow_mut() = Some(Status::done(note));
        }
        Err(error) => *status.borrow_mut() = Some(Status::error(error)),
    }
    cx.refresh_windows();
}

/// 点「移除」：预览清空，标记「保存时要删封面」。
fn remove_cover(cx: &mut App) {
    let Some((cover, dirty, status)) = cx.try_global::<MetadataEditor>().map(|editor| {
        (
            editor.cover.clone(),
            editor.cover_dirty.clone(),
            editor.status.clone(),
        )
    }) else {
        return;
    };

    *cover.borrow_mut() = None;
    dirty.set(true);
    *status.borrow_mut() = Some(Status::done("保存后会把文件里的封面删掉"));
    cx.refresh_windows();
}

/// 只取文件名；拿不到就退回完整路径。
fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// 点「匹配元数据」：拿标题 + 艺术家去音源搜，结果回到 [`apply_match`]。
fn start_match(cx: &mut App) {
    let Some((fields, status)) = cx
        .try_global::<MetadataEditor>()
        .map(|editor| (editor.fields.clone(), editor.status.clone()))
    else {
        return;
    };

    let title = fields[0].read(cx).value().to_string();
    let artist = fields[1].read(cx).value().to_string();
    let keyword = title.trim().to_string();
    if keyword.is_empty() {
        *status.borrow_mut() = Some(Status::error("先填写歌曲标题，再点匹配"));
        cx.refresh_windows();
        return;
    }

    *status.borrow_mut() = Some(Status::busy(format!("正在搜索「{keyword}」…")));
    cx.refresh_windows();

    // 音源请求是同步阻塞的，扔到后台执行器，别卡住界面。
    let task = cx
        .background_executor()
        .spawn(async move { lx::match_track(&keyword, Some(artist.as_str())) });

    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| apply_match(cx, result));
    })
    .detach();
}

/// 把音源结果填进表单，歌词先存着等「保存」再落盘。
fn apply_match(cx: &mut App, result: Result<lx::MatchedTrack, String>) {
    let Some((fields, status, pending)) = cx.try_global::<MetadataEditor>().map(|editor| {
        (
            editor.fields.clone(),
            editor.status.clone(),
            editor.pending_lyric.clone(),
        )
    }) else {
        return;
    };

    let track = match result {
        Ok(track) => track,
        Err(error) => {
            *status.borrow_mut() = Some(Status::error(error));
            cx.refresh_windows();
            return;
        }
    };

    // 下标对应 `FIELDS`：标题 / 艺术家 / 专辑 / 年份 / 流派 / 音轨号；音源没给的字段留空不动。
    let updates = [
        (0, track.title.clone()),
        (1, track.artist.clone()),
        (2, track.album.clone()),
        (4, track.year.clone()),
        (5, track.genre.clone()),
        (6, track.track_no.clone()),
    ];
    if let Some(window) = cx.active_window() {
        let _ = window.update(cx, |_, window, cx| {
            for (index, value) in updates.iter() {
                if value.is_empty() {
                    continue;
                }
                fields[*index].update(cx, |state, cx| state.set_value(value.clone(), window, cx));
            }
        });
    }

    // 歌词先挂在弹窗上，点「保存」时才写进音频旁边的 `.lrc`。
    let note = match track.lyric.clone() {
        Some(lyric) => {
            *pending.borrow_mut() = Some(lyric);
            format!(
                "已匹配：{} - {}（{}） · 歌词已取到，保存后写入 .lrc",
                track.artist, track.title, track.provider
            )
        }
        None => {
            *pending.borrow_mut() = None;
            format!(
                "已匹配：{} - {}（{}） · 音源没有提供歌词",
                track.artist, track.title, track.provider
            )
        }
    };
    *status.borrow_mut() = Some(Status::done(note));
    cx.refresh_windows();
}

/// 把表单写进音频文件；成功返回 `true`（弹窗可以关），失败返回 `false` 并留下原因。
fn save(window: &mut Window, cx: &mut App) -> bool {
    let Some((file_slot, fields, pending, status, cover, cover_dirty)) =
        cx.try_global::<MetadataEditor>().map(|editor| {
            (
                editor.file.clone(),
                editor.fields.clone(),
                editor.pending_lyric.clone(),
                editor.status.clone(),
                editor.cover.clone(),
                editor.cover_dirty.clone(),
            )
        })
    else {
        return false;
    };
    let path = file_slot.borrow().clone();
    let values: Vec<String> = fields
        .iter()
        .map(|field| field.read(cx).value().to_string())
        .collect();

    let Some(path) = path else {
        return false;
    };

    let tags = tags::Tags {
        title: values.first().cloned().unwrap_or_default(),
        artist: values.get(1).cloned().unwrap_or_default(),
        album: values.get(2).cloned().unwrap_or_default(),
        album_artist: values.get(3).cloned().unwrap_or_default(),
        year: values.get(4).cloned().unwrap_or_default(),
        genre: values.get(5).cloned().unwrap_or_default(),
        track_no: values.get(6).cloned().unwrap_or_default(),
        disc_no: values.get(7).cloned().unwrap_or_default(),
    };

    // 没动过封面就不写：`Keep` 让文件里原有的图原样留着。
    // 借用要一直活到写文件结束，所以先把 guard 存下来。
    let cover_guard = cover.borrow();
    let change = if cover_dirty.get() {
        match cover_guard.as_ref() {
            Some(view) => tags::CoverChange::Set(&view.cover),
            None => tags::CoverChange::Remove,
        }
    } else {
        tags::CoverChange::Keep
    };

    // 先写文件：失败就什么都不改，歌词和封面也还挂在弹窗上等下一次保存。
    if let Err(error) = tags::write_all(&path, &tags, change) {
        *status.borrow_mut() = Some(Status::error(error));
        cx.refresh_windows();
        return false;
    }
    cover_dirty.set(false);
    drop(cover_guard);

    // 标签写成功了，再落盘音源匹配到的歌词（取消就一起丢掉）。
    if let Some(lyric) = pending.borrow_mut().take() {
        let _ = lx::write_lyric(&path, &lyric);
    }

    // 列表显示跟着文件里的标签走，不用重新扫描。
    let mut state = library::state(cx);
    for file in state.files.iter_mut() {
        if file.path != path {
            continue;
        }
        let title = tags.title.trim();
        if !title.is_empty() {
            file.title = title.to_string();
        }
        let artist = tags.artist.trim();
        file.artist = (!artist.is_empty()).then(|| artist.to_string());
        let album = tags.album.trim();
        file.album = (!album.is_empty()).then(|| album.to_string());
    }
    cx.set_global(state);

    *status.borrow_mut() = None;
    window.refresh();
    true
}

/// 标签里有值就用标签的，否则退回音乐库推断的值。
fn pick(preferred: String, fallback: String) -> String {
    if preferred.trim().is_empty() {
        fallback
    } else {
        preferred
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{Context, Render, TestAppContext};

    /// 把弹窗挂在窗口根上，位置和主窗口里一致。
    struct Host;

    impl Render for Host {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(dialog(window, cx))
        }
    }

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::current_dir().expect("工作目录").join("target");
        std::fs::create_dir_all(&dir).expect("建 target");
        let path = dir.join(name);
        tags::write_test_wav(&path);
        path
    }

    fn audio_file(path: &Path) -> AudioFile {
        AudioFile {
            path: path.to_path_buf(),
            title: "测试歌曲".into(),
            artist: Some("测试歌手".into()),
            album: None,
            extension: "wav".into(),
            size: 0,
        }
    }

    fn png_cover() -> CoverView {
        CoverView::new(
            tags::Cover {
                bytes: tags::TINY_PNG.to_vec(),
                mime: "image/png".into(),
            },
            "封面.png".into(),
        )
    }

    /// 文件大小按 1024 进制、MB 保留一位小数。
    #[test]
    fn cover_sizes_read_like_a_file_manager() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(900), "900 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(3 * 1024 * 1024 / 2), "1.5 MB");
        assert_eq!(
            png_cover().summary(),
            format!("封面.png · {}", human_size(tags::TINY_PNG.len())),
            "摘要应是「名字 · 大小」"
        );
    }

    /// 选图 → 预览 → 保存进文件；再打开 → 移除 → 保存把封面删掉。
    #[gpui_kit::test]
    fn the_cover_round_trips_through_the_dialog(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let path = fixture("metadata-cover-dialog.wav");
        let file = audio_file(&path);

        let (_, cx) = cx.add_window_view(|window, cx| {
            init(window, cx);
            Host
        });

        cx.update(|window, cx| open(window, cx, &file));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("metadata-cover-empty").is_some(),
            "没有封面时应显示空占位"
        );
        assert!(
            cx.debug_bounds("metadata-cover-remove").is_none(),
            "没有封面时不该有「移除」"
        );

        // 真实的「选文件」要走系统对话框，测试里直接把读图结果塞进来。
        cx.update(|_, cx| apply_cover(cx, Ok(png_cover())));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("metadata-cover-filled").is_some(),
            "选好图后应显示预览"
        );
        assert_eq!(tags::read_cover(&path), None, "点保存之前文件里还不该有封面");

        cx.update(|window, cx| assert!(save(window, cx), "保存应成功"));
        let written = tags::read_cover(&path).expect("保存后文件里应有封面");
        assert_eq!(written.bytes, tags::TINY_PNG.to_vec(), "写进去的应是刚选的那张图");

        // 重新打开：预览来自文件，而不是内存里剩下的状态。
        cx.update(|window, cx| open(window, cx, &file));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("metadata-cover-filled").is_some(),
            "打开带封面的歌应看到预览"
        );

        let remove = cx
            .debug_bounds("metadata-cover-remove")
            .expect("有封面时应能移除");
        cx.simulate_click(remove.center(), Default::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("metadata-cover-empty").is_some(),
            "点「移除」后应回到空占位"
        );

        cx.update(|window, cx| assert!(save(window, cx), "保存应成功"));
        assert_eq!(tags::read_cover(&path), None, "移除后保存应把封面删掉");
    }
}
