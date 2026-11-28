//! 歌词编辑器的窗口内容。
//!
//! 从上到下四段：菜单栏（文件 / 编辑 / 预览）、插入栏（新歌词 / 撤销 / 重做）、
//! 两列表格（时间、歌词内容）、状态栏。编辑器内部仍以 LRC 为工作格式，
//! 导入的 TTML 会先转成 LRC 再铺进表格（见 [`super::format`]）。
//!
//! 从音乐库右键「修改歌词」进来时带着一首歌（[`super::Song`]）：有歌词就载入，
//! 没有就先弹提示框问用户是新建还是从音源下载；写回时落到音频旁边的 `.lrc`
//! 与文件内嵌歌词标签。

use std::path::{Path, PathBuf};

use crate::{
    audio,
    services::{lx, tags},
    ui::{
        button::{Button, ButtonSize, ButtonVariant, style_button},
        dialog::{DialogHandle, dialog_footer, dialog_popup, dialog_title, dialog_with},
        input::{Input, InputEvent, InputState},
        menu::{Menu, MenuItem, MenuState},
        theme::UiTheme,
    },
};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement as _, Styled, WeakEntity, Window, div, px,
};

use super::{Song, format, window::TITLE};

/// 时间列的宽度。
const TIME_COLUMN_WIDTH: f32 = 112.;
/// 表格行高。
const ROW_HEIGHT: f32 = 38.;

/// 导出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportKind {
    /// 纯 LRC 文本。
    Lrc,
    /// TTML（每句 begin/end）。
    Ttml,
}

impl ExportKind {
    /// 文件扩展名。
    fn ext(self) -> &'static str {
        match self {
            Self::Lrc => "lrc",
            Self::Ttml => "ttml",
        }
    }
}

/// 一行歌词在撤销栈里的纯文本快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RowValue {
    /// 时间列，`mm:ss.xx`。
    time: String,
    /// 歌词内容。
    text: String,
}

impl RowValue {
    fn is_empty(&self) -> bool {
        self.time.trim().is_empty() && self.text.trim().is_empty()
    }
}

/// 表格的一行：时间与内容各一个单行输入框。
struct LyricRow {
    time: Entity<InputState>,
    text: Entity<InputState>,
}

/// 「使用播放器预览」时抛出的事件，供播放器侧订阅。
#[derive(Debug, Clone)]
pub struct PreviewRequest {
    /// 当前表格内容拼成的 LRC 文本。
    pub lyrics: String,
}

/// 歌词编辑器的根视图。
pub struct LyricsEditorView {
    /// 表格行。
    rows: Vec<LyricRow>,
    /// 已提交的内容快照，撤销 / 重做以它为基准。
    current: Vec<RowValue>,
    /// 撤销栈（越靠后越新）。
    history: Vec<Vec<RowValue>>,
    /// 重做栈。
    future: Vec<Vec<RowValue>>,
    /// 正在回填内容时为真，避免把程序写入记成一次编辑。
    applying: bool,
    /// 顶部三个下拉菜单的保留状态。
    file_menu: Entity<MenuState>,
    edit_menu: Entity<MenuState>,
    preview_menu: Entity<MenuState>,
    /// 当前文件名（导入或导出后更新）。
    doc_name: Option<SharedString>,
    /// 最近一次操作的结果提示。
    status: Option<SharedString>,
    /// 上次导入 / 导出的目录，作为下次文件对话框的起点。
    last_dir: Option<PathBuf>,
    /// 正在为哪首歌编辑；`None` 是托盘打开的空白编辑器，没有可写回的文件。
    song: Option<Song>,
    /// 「这首歌还没有歌词」提示框。
    notice: DialogHandle,
}

impl EventEmitter<PreviewRequest> for LyricsEditorView {}

impl LyricsEditorView {
    /// 新建编辑器视图；`window` 用于创建输入状态，`song` 是这次要为哪首歌编辑。
    pub fn new(window: &mut Window, cx: &mut Context<Self>, song: Option<Song>) -> Self {
        let this = cx.weak_entity();
        let file_menu = cx.new(|cx| MenuState::new(file_items(this.clone()), cx));
        let edit_menu = cx.new(|cx| MenuState::new(edit_items(this.clone()), cx));
        let preview_menu = cx.new(|cx| MenuState::new(preview_items(this), cx));

        let mut view = Self {
            rows: Vec::new(),
            current: vec![RowValue::default()],
            history: Vec::new(),
            future: Vec::new(),
            applying: false,
            file_menu,
            edit_menu,
            preview_menu,
            doc_name: None,
            status: None,
            last_dir: None,
            song: None,
            notice: DialogHandle::new(false),
        };
        // 起手给一行空白，用户可以直接开始写。
        view.build_rows(1, window, cx);
        view.attach(song, window, cx);
        view
    }

    // ---- 歌曲绑定 ----

    /// 切到某首歌：有歌词就铺进表格，没有就弹提示框问歌词从哪来。
    ///
    /// `song` 为 `None` 时只是把窗口提到最前，不动用户正在编辑的内容。
    pub fn attach(&mut self, song: Option<Song>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(song) = song else {
            return;
        };

        let rows = read_rows(&song.lyric_path());
        // 换歌等于换文档：撤销栈留着会撤回到上一首的歌词。
        self.history.clear();
        self.future.clear();
        self.song = Some(song.clone());
        self.doc_name = Some(SharedString::from(song.title.clone()));
        window.set_window_title(&format!("{TITLE} · {}", song.title));

        if rows.is_empty() {
            // 先把表格清成空白，用户在提示框里选「新建」就能直接写。
            self.set_rows(vec![RowValue::default()], window, cx);
            self.set_status(format!("{} 还没有歌词", song.display()), cx);
            self.notice.open(window, cx);
        } else {
            self.set_rows(rows, window, cx);
            self.set_status(
                format!("已载入{}的歌词，改完点「保存到歌曲」", song.display()),
                cx,
            );
        }
    }

    // ---- 内容模型 ----

    /// 读一行的当前文本。
    fn value_of(row: &LyricRow, cx: &App) -> RowValue {
        RowValue {
            time: row.time.read(cx).value().to_string(),
            text: row.text.read(cx).value().to_string(),
        }
    }

    /// 读表格里所有行的当前文本。
    fn values(&self, cx: &App) -> Vec<RowValue> {
        self.rows
            .iter()
            .map(|row| Self::value_of(row, cx))
            .collect()
    }

    /// 用给定的快照重建表格（行数变了就补行 / 删行）。
    fn set_rows(&mut self, values: Vec<RowValue>, window: &mut Window, cx: &mut Context<Self>) {
        self.applying = true;

        while self.rows.len() < values.len() {
            let row = self.create_row(window, cx);
            self.rows.push(row);
        }
        self.rows.truncate(values.len());

        for (row, value) in self.rows.iter().zip(&values) {
            row.time.update(cx, |state, cx| {
                state.set_value(value.time.clone(), window, cx)
            });
            row.text.update(cx, |state, cx| {
                state.set_value(value.text.clone(), window, cx)
            });
        }

        self.current = values;
        self.applying = false;
        cx.notify();
    }

    /// 直接把行数补齐到 `count` 行空表格（不经过撤销栈）。
    fn build_rows(&mut self, count: usize, window: &mut Window, cx: &mut Context<Self>) {
        while self.rows.len() < count {
            let row = self.create_row(window, cx);
            self.rows.push(row);
        }
        self.rows.truncate(count);
    }

    /// 建一行空输入框，并把后续输入接到「记录一次编辑」上。
    fn create_row(&self, window: &mut Window, cx: &mut Context<Self>) -> LyricRow {
        let time = self.create_input("00:00.00", window, cx);
        let text = self.create_input("歌词内容", window, cx);
        LyricRow { time, text }
    }

    fn create_input(
        &self,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(SharedString::from(placeholder.to_string()))
        });
        cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && !this.applying {
                this.record(cx);
            }
        })
        .detach();
        input
    }

    /// 把当前内容与上次提交比对，有变化就压进撤销栈。
    fn record(&mut self, cx: &mut Context<Self>) {
        let next = self.values(cx);
        if next == self.current {
            return;
        }
        let previous = std::mem::replace(&mut self.current, next);
        self.history.push(previous);
        self.future.clear();
        cx.notify();
    }

    /// 撤销一步。
    fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.applying {
            return;
        }
        // 输入框里的最后一次改动可能还没进栈，先补记。
        self.record(cx);
        let Some(previous) = self.history.pop() else {
            self.set_status("没有可撤销的修改", cx);
            return;
        };
        self.future.push(self.current.clone());
        self.set_rows(previous, window, cx);
        self.set_status("已撤销", cx);
    }

    /// 重做一步。
    fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.applying {
            return;
        }
        let Some(next) = self.future.pop() else {
            self.set_status("没有可重做的修改", cx);
            return;
        };
        self.history.push(self.current.clone());
        self.set_rows(next, window, cx);
        self.set_status("已重做", cx);
    }

    /// 清空成一行空白歌词。
    fn new_lyrics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.record(cx);
        if self.current.len() == 1 && self.current[0].is_empty() {
            self.set_status("已经是空白歌词了", cx);
            return;
        }
        self.history.push(self.current.clone());
        self.future.clear();
        self.set_rows(vec![RowValue::default()], window, cx);
        // 绑定了歌曲就仍算这首歌的歌词文件，名字不改。
        self.doc_name = self
            .song
            .as_ref()
            .map(|song| SharedString::from(song.title.clone()));
        self.set_status("已新建空白歌词", cx);
    }

    /// 提示框里选「为这首歌新建歌词」：留一张空白表格，光标落到第一行。
    fn start_new_lyrics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history.clear();
        self.future.clear();
        self.set_rows(vec![RowValue::default()], window, cx);
        self.set_status("写好后点「保存到歌曲」写进这首歌", cx);
        if let Some(row) = self.rows.first() {
            let focus = Input::new(&row.text).field_focus_handle(cx);
            window.focus(&focus, cx);
        }
    }

    /// 在末尾追加一行。
    fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.record(cx);
        let mut values = self.current.clone();
        values.push(RowValue::default());
        self.history.push(self.current.clone());
        self.future.clear();
        self.set_rows(values, window, cx);
        self.set_status("已添加一行", cx);
    }

    /// 把表格内容拼成 LRC 文本；没填内容的行为会被跳过。
    fn lrc_text(&self, cx: &App) -> String {
        let mut lines: Vec<String> = Vec::new();
        for value in self.values(cx) {
            let text = value.text.trim();
            if text.is_empty() {
                continue;
            }
            let time = format::parse_time(&value.time)
                .map(format::format_time)
                .unwrap_or_else(|| "00:00.00".to_string());
            lines.push(format!("[{time}]{text}"));
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        }
    }

    fn set_status(&mut self, status: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = Some(status.into());
        cx.notify();
    }

    // ---- 文件 ----

    /// 弹系统文件对话框，把选中的歌词读进表格。
    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择歌词文件（.lrc / .ttml）".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            let raw = match std::fs::read_to_string(&path) {
                Ok(raw) => raw,
                Err(error) => {
                    let message = format!("读取失败：{error}");
                    cx.update(|_, app| {
                        this.update(app, |view, cx| view.set_status(message, cx))
                            .ok();
                    })
                    .ok();
                    return;
                }
            };

            let values = rows_from_text(&raw);
            let values = if values.is_empty() {
                vec![RowValue::default()]
            } else {
                values
            };
            let message = format!("已导入 {} 行", values.len());
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string());

            cx.update(|window, app| {
                this.update(app, |view, cx| {
                    view.history.push(view.current.clone());
                    view.future.clear();
                    view.set_rows(values, window, cx);
                    view.doc_name = name.map(SharedString::from);
                    view.last_dir = path.parent().map(Path::to_path_buf);
                    view.set_status(message, cx);
                })
                .ok();
            })
            .ok();
        })
        .detach();
    }

    /// 导出当前歌词：先选路径，再按格式写文件。
    fn export(&mut self, kind: ExportKind, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.lrc_text(cx);
        let stem = self
            .doc_name
            .clone()
            .map(|name| name.to_string())
            .unwrap_or_else(|| "lyrics".to_string());
        let suggested = format::with_extension(&stem, kind.ext());
        let directory = self.last_dir.clone().unwrap_or_else(home_directory);

        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };

            let payload = match kind {
                ExportKind::Lrc => text,
                ExportKind::Ttml => format::to_ttml(&text),
            };
            let message = match std::fs::write(&path, payload) {
                Ok(()) => format!("已导出 {}", path.display()),
                Err(error) => format!("导出失败：{error}"),
            };

            cx.update(|_, app| {
                this.update(app, |view, cx| {
                    view.last_dir = path.parent().map(Path::to_path_buf);
                    view.doc_name = path
                        .file_name()
                        .map(|name| SharedString::from(name.to_string_lossy().to_string()));
                    view.set_status(message, cx);
                })
                .ok();
            })
            .ok();
        })
        .detach();
    }

    // ---- 写回歌曲 ----

    /// 把表格内容写进当前歌曲：旁边的 `.lrc` 给播放器用，内嵌歌词标签随身携带。
    fn write_to_song(&mut self, cx: &mut Context<Self>) -> Result<String, String> {
        let Some(song) = self.song.clone() else {
            return Err("编辑器还没绑定歌曲，先右键某首歌「修改歌词」".into());
        };
        let text = self.lrc_text(cx);
        if text.trim().is_empty() {
            return Err("还一句歌词都没写，先写点内容".into());
        }

        let path = lx::write_lyric(&song.path, &text)?;
        // 内嵌歌词是加分项：格式不支持时不影响 `.lrc` 已经落盘。
        let embedded = tags::write_lyric(&song.path, &text).is_ok();
        // 正在放这首的话，浮动栏应该立刻跟上新歌词。
        audio::reload_lyrics_if_playing(cx, &song.path);

        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok(if embedded {
            format!("已写进歌曲：{name} + 文件内嵌歌词")
        } else {
            format!("已写进歌曲：{name}")
        })
    }

    /// 工具栏 / 文件菜单里的「保存到歌曲」：结果只写在状态栏。
    fn save_to_song(&mut self, cx: &mut Context<Self>) {
        match self.write_to_song(cx) {
            Ok(note) => self.set_status(note, cx),
            Err(error) => self.set_status(error, cx),
        }
    }

    /// 提示框里选「从音源下载歌词」：后台搜，回来了再铺进表格并写进歌曲。
    fn download_lyric(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(song) = self.song.clone() else {
            return;
        };
        self.set_status(format!("正在从音源搜索{}…", song.display()), cx);

        let title = song.title.clone();
        let artist = song.artist.clone();
        // 音源请求是同步阻塞的，扔到后台执行器，别卡住界面。
        let task = cx
            .background_executor()
            .spawn(async move { lx::match_track(&title, artist.as_deref()) });

        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            cx.update(|window, app| {
                this.update(app, |view, cx| view.apply_download(result, window, cx))
                    .ok();
            })
            .ok();
        })
        .detach();
    }

    /// 音源结果回来：铺进表格、写进歌曲，结果写进状态栏。
    fn apply_download(
        &mut self,
        result: Result<lx::MatchedTrack, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let track = match result {
            Ok(track) => track,
            Err(error) => {
                self.set_status(format!("下载歌词失败：{error}"), cx);
                return;
            }
        };

        let Some(lyric) = track
            .lyric
            .clone()
            .filter(|lyric| !lyric.trim().is_empty())
        else {
            self.set_status(format!("{} 没有提供这首歌的歌词", track.provider), cx);
            return;
        };
        let rows = rows_from_text(&lyric);
        if rows.is_empty() {
            self.set_status("音源给的歌词没有时间标签，读不出来", cx);
            return;
        }

        self.history.clear();
        self.future.clear();
        self.set_rows(rows, window, cx);
        let message = match self.write_to_song(cx) {
            Ok(note) => format!("已从 {} 下载：{note}", track.provider),
            Err(error) => format!("歌词已下载，但写入失败：{error}"),
        };
        self.set_status(message, cx);
    }

    // ---- 没有歌词时的提示框 ----

    /// 「这首歌还没有歌词」提示框：新建 / 从音源下载 / 取消。
    fn notice_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(song) = self.song.clone() else {
            return div().into_any_element();
        };
        let handle = self.notice.clone();
        let message = format!("当前歌曲{}还没有歌词，你想要？", song.display());

        let cancel_handle = handle.clone();
        let cancel_this = cx.entity();
        let download_handle = handle.clone();
        let download_this = cx.entity();
        let create_handle = handle.clone();
        let create_this = cx.entity();

        dialog_with(
            "lyrics-notice",
            &self.notice,
            move |_window, cx| {
                let theme = UiTheme::read(cx).clone();

                dialog_popup("lyrics-notice-popup", "还没有歌词", cx)
                    .debug_selector(|| "lyrics-notice-popup".into())
                    .max_w(px(460.))
                    .child(dialog_title("lyrics-notice-title", cx).child("这首歌还没有歌词"))
                    .child(
                        div()
                            .w_full()
                            .text_size(px(13.))
                            .line_height(px(21.))
                            .text_color(theme.colors.foreground)
                            .child(message.clone()),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_size(px(12.))
                            .line_height(px(19.))
                            .text_color(theme.colors.muted_foreground)
                            .child("新建就留一张空白表格，写好后点「保存到歌曲」才会写进这首歌。"),
                    )
                    .child(
                        dialog_footer(cx)
                            .child(
                                Button::new("lyrics-notice-cancel")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Sm)
                                    .child("取消")
                                    .on_click(move |_, window, cx| {
                                        cancel_handle.close(window, cx);
                                        cancel_this.update(cx, |view, cx| {
                                            view.set_status("已取消，也可以从「文件 → 导入文件…」拿歌词", cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("lyrics-notice-download")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Sm)
                                    .child("从音源下载歌词")
                                    .on_click(move |_, window, cx| {
                                        download_handle.close(window, cx);
                                        download_this.update(cx, |view, cx| {
                                            view.download_lyric(window, cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("lyrics-notice-create")
                                    .variant(ButtonVariant::Default)
                                    .size(ButtonSize::Sm)
                                    .child("为这首歌新建歌词")
                                    .on_click(move |_, window, cx| {
                                        create_handle.close(window, cx);
                                        create_this.update(cx, |view, cx| {
                                            view.start_new_lyrics(window, cx)
                                        });
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

    /// 把当前歌词交给播放器预览；播放器侧订阅 [`PreviewRequest`] 即可接手。
    fn preview_with_player(&mut self, cx: &mut Context<Self>) {
        let lyrics = self.lrc_text(cx);
        if lyrics.trim().is_empty() {
            self.set_status("表格还是空的，先写几句再预览", cx);
            return;
        }
        cx.emit(PreviewRequest { lyrics });
        self.set_status("已把当前歌词交给播放器预览", cx);
    }

    // ---- 视图 ----

    /// 顶部菜单栏：文件 / 编辑 / 预览，右侧显示当前文档名。
    fn menu_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let document = self
            .doc_name
            .clone()
            .unwrap_or_else(|| SharedString::from("未命名歌词"));

        let document_label = div()
            .pr(px(8.))
            .text_size(theme.text(12.))
            .text_color(theme.colors.muted_foreground)
            .child(document);

        div()
            .flex_shrink_0()
            .h(px(40.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(2.))
            .border_b_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.background)
            .child(menu_trigger(&self.file_menu, "文件", &theme))
            .child(menu_trigger(&self.edit_menu, "编辑", &theme))
            .child(menu_trigger(&self.preview_menu, "预览", &theme))
            .child(div().flex_1())
            .child(document_label)
    }

    /// 插入栏：新歌词 / 撤销 / 重做，都是带图标的按钮。
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let this = cx.weak_entity();

        let new_lyrics = this.clone();
        let undo = this.clone();
        let redo = this;

        let new_button = toolbar_button(
            "lyrics-toolbar-new",
            LucideIcon::FilePlus,
            "新歌词",
            &theme,
            move |window, cx| {
                with_view(&new_lyrics, cx, |view, cx| view.new_lyrics(window, cx));
            },
        );
        let undo_button = toolbar_button(
            "lyrics-toolbar-undo",
            LucideIcon::Undo2,
            "撤销",
            &theme,
            move |window, cx| {
                with_view(&undo, cx, |view, cx| view.undo(window, cx));
            },
        );
        let redo_button = toolbar_button(
            "lyrics-toolbar-redo",
            LucideIcon::Redo2,
            "重做",
            &theme,
            move |window, cx| {
                with_view(&redo, cx, |view, cx| view.redo(window, cx));
            },
        );

        let mut bar = div()
            .flex_shrink_0()
            .h(px(44.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(4.))
            .border_b_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.background)
            .child(new_button)
            .child(undo_button)
            .child(redo_button);

        // 绑定了歌曲才谈得上「写回去」，空白编辑器不显示这个按钮。
        if let Some(song) = &self.song {
            let save = cx.weak_entity();
            let song_label = div()
                .min_w_0()
                .truncate()
                .text_size(theme.text(11.))
                .text_color(theme.colors.muted_foreground)
                .child(format!("正在编辑：{}", song.display()));

            bar = bar
                .child(
                    div()
                        .w(px(1.))
                        .h(px(18.))
                        .mx(px(4.))
                        .bg(theme.colors.border),
                )
                .child(toolbar_button(
                    "lyrics-toolbar-save-song",
                    LucideIcon::Save,
                    "保存到歌曲",
                    &theme,
                    move |_window, cx| {
                        with_view(&save, cx, |view, cx| view.save_to_song(cx));
                    },
                ))
                .child(div().flex_1())
                .child(song_label);
        }

        bar
    }

    /// 两列表格：时间 / 歌词内容。
    fn table(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let header = self.table_header(&theme);

        let rows: Vec<_> = self
            .rows
            .iter()
            .map(|row| self.table_row(row, &theme))
            .collect();

        let body = div()
            .id("lyrics-sheet-rows")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .children(rows);

        let footer = self.table_footer(&theme, cx);

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded(theme.radius.md)
            .border_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.card)
            .overflow_hidden()
            .child(header)
            .child(body)
            .child(footer)
    }

    fn table_header(&self, theme: &UiTheme) -> impl IntoElement {
        let time_label = div()
            .text_size(theme.text(11.))
            .text_color(theme.colors.muted_foreground)
            .child("时间");
        let text_label = div()
            .text_size(theme.text(11.))
            .text_color(theme.colors.muted_foreground)
            .child("歌词内容");

        div()
            .flex_shrink_0()
            .h(px(32.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_b_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.muted)
            .child(
                div()
                    .w(px(TIME_COLUMN_WIDTH))
                    .flex_shrink_0()
                    .child(time_label),
            )
            .child(div().flex_1().min_w_0().child(text_label))
    }

    fn table_row(&self, row: &LyricRow, theme: &UiTheme) -> impl IntoElement {
        let time_cell = div()
            .w(px(TIME_COLUMN_WIDTH))
            .flex_shrink_0()
            .child(Input::new(&row.time).bordered(false).aria_label("时间"));

        let text_cell = div()
            .flex_1()
            .min_w_0()
            .child(Input::new(&row.text).bordered(false).aria_label("歌词内容"));

        div()
            .h(px(ROW_HEIGHT))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_b_1()
            .border_color(theme.colors.border)
            .child(time_cell)
            .child(text_cell)
    }

    fn table_footer(&self, theme: &UiTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let add = cx.weak_entity();
        let add_label = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(lucide(LucideIcon::Plus).size(px(13.)))
            .child("添加一行");

        let add_button = Button::new("lyrics-add-row")
            .size(ButtonSize::Xs)
            .variant(ButtonVariant::Ghost)
            .aria_label("添加一行")
            .child(add_label)
            .on_click(move |_, window, cx| {
                with_view(&add, cx, |view, cx| view.add_row(window, cx));
            });

        let count = div()
            .text_size(theme.text(11.))
            .text_color(theme.colors.muted_foreground)
            .child(format!("共 {} 行", self.rows.len()));

        div()
            .flex_shrink_0()
            .h(px(36.))
            .px(px(8.))
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(theme.colors.border)
            .child(add_button)
            .child(count)
    }

    /// 底部状态栏：左边是格式提示，右边是最近一次操作结果。
    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let status = self
            .status
            .clone()
            .unwrap_or_else(|| SharedString::from("时间写成 mm:ss.xx，例如 01:23.45"));

        let hint = div()
            .flex_shrink_0()
            .text_size(theme.text(11.))
            .text_color(theme.colors.muted_foreground)
            .child("LRC 工作格式，可导入 / 导出 TTML");

        div()
            .flex_shrink_0()
            .h(px(26.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .border_t_1()
            .border_color(theme.colors.border)
            .text_size(theme.text(11.))
            .text_color(theme.colors.muted_foreground)
            .child(hint)
            .child(status)
    }
}

impl Render for LyricsEditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let sheet = div().flex_1().min_h_0().p(px(12.)).child(self.table(cx));
        let notice = self.notice_dialog(window, cx);

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme.colors.background)
            .text_color(theme.colors.foreground)
            .font_family(theme.fonts.body.clone())
            .child(self.menu_bar(cx))
            .child(self.toolbar(cx))
            .child(sheet)
            .child(self.status_bar(cx))
            // 提示框挂在最外层，和主窗口的弹窗同一层。
            .child(notice)
    }
}

/// 用 `Ghost` 小按钮画一个菜单入口，看起来就是菜单栏上的文字。
fn menu_trigger(
    state: &Entity<MenuState>,
    label: &'static str,
    theme: &UiTheme,
) -> impl IntoElement {
    let theme = theme.clone();
    Menu::new(state, label).trigger_style_with(move |button| {
        style_button(button, false, ButtonVariant::Ghost, ButtonSize::Xs, &theme)
    })
}

/// 插入栏按钮：图标 + 文字并排。
fn toolbar_button(
    id: &'static str,
    icon: LucideIcon,
    label: &'static str,
    theme: &UiTheme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let icon_element = lucide(icon)
        .size(px(15.))
        .text_color(theme.colors.muted_foreground);
    let label_element = div().text_size(theme.text(12.)).child(label);

    let content = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(icon_element)
        .child(label_element);

    Button::new(id)
        .size(ButtonSize::Sm)
        .variant(ButtonVariant::Ghost)
        .aria_label(label)
        .child(content)
        .on_click(move |_, window, cx| on_click(window, cx))
}

/// 「文件」菜单：导入、导出（LRC / TTML）、保存到歌曲。
fn file_items(this: WeakEntity<LyricsEditorView>) -> Vec<MenuItem> {
    let import = this.clone();
    let export_lrc = this.clone();
    let export_ttml = this.clone();
    let save_song = this;

    vec![
        MenuItem::new("lyrics-import", "导入文件…")
            .icon(LucideIcon::FolderOpen)
            .shortcut("secondary-o")
            .on_click(move |_, window, cx| {
                with_view(&import, cx, |view, cx| view.import(window, cx));
            }),
        MenuItem::separator(),
        MenuItem::submenu(
            "lyrics-export",
            "导出文件",
            vec![
                MenuItem::new("lyrics-export-lrc", "导出为 LRC")
                    .icon(LucideIcon::FileText)
                    .on_click(move |_, window, cx| {
                        with_view(&export_lrc, cx, |view, cx| {
                            view.export(ExportKind::Lrc, window, cx)
                        });
                    }),
                MenuItem::new("lyrics-export-ttml", "导出为 TTML")
                    .icon(LucideIcon::FileCode)
                    .on_click(move |_, window, cx| {
                        with_view(&export_ttml, cx, |view, cx| {
                            view.export(ExportKind::Ttml, window, cx)
                        });
                    }),
            ],
        ),
        MenuItem::separator(),
        // 只有从音乐库带歌进来时才有意义，空白编辑器点了会在状态栏提示。
        MenuItem::new("lyrics-save-song", "保存到歌曲")
            .icon(LucideIcon::Save)
            .shortcut("secondary-s")
            .on_click(move |_, _, cx| {
                with_view(&save_song, cx, |view, cx| view.save_to_song(cx));
            }),
    ]
}

/// 「编辑」菜单：撤销、重做。
fn edit_items(this: WeakEntity<LyricsEditorView>) -> Vec<MenuItem> {
    let undo = this.clone();
    let redo = this;

    vec![
        MenuItem::new("lyrics-undo", "撤销")
            .icon(LucideIcon::Undo2)
            .shortcut("secondary-z")
            .on_click(move |_, window, cx| {
                with_view(&undo, cx, |view, cx| view.undo(window, cx));
            }),
        MenuItem::new("lyrics-redo", "重做")
            .icon(LucideIcon::Redo2)
            .shortcut("secondary-shift-z")
            .on_click(move |_, window, cx| {
                with_view(&redo, cx, |view, cx| view.redo(window, cx));
            }),
    ]
}

/// 「预览」菜单：交给播放器预览。
fn preview_items(this: WeakEntity<LyricsEditorView>) -> Vec<MenuItem> {
    vec![
        MenuItem::new("lyrics-preview-player", "使用播放器预览")
            .icon(LucideIcon::Play)
            .on_click(move |_, _, cx| {
                with_view(&this, cx, |view, cx| view.preview_with_player(cx));
            }),
    ]
}

/// 菜单项回调里拿回视图实体。
fn with_view(
    this: &WeakEntity<LyricsEditorView>,
    cx: &mut App,
    f: impl FnOnce(&mut LyricsEditorView, &mut Context<LyricsEditorView>),
) {
    if let Some(view) = this.upgrade() {
        view.update(cx, |view, cx| f(view, cx));
    }
}

/// 「另存为」的默认目录：先看 `$HOME`，再退回当前目录。
fn home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

/// LRC / TTML 文本 → 表格行；没有时间标签的文本会被丢掉。
fn rows_from_text(raw: &str) -> Vec<RowValue> {
    format::parse_lrc(&format::import_text(raw))
        .into_iter()
        .map(|line| RowValue {
            time: format::format_time(line.time_ms),
            text: line.text,
        })
        .collect()
}

/// 读一个歌词文件并转成表格行；文件不存在、读不出来、没有时间标签都返回空。
fn read_rows(path: &Path) -> Vec<RowValue> {
    match std::fs::read_to_string(path) {
        Ok(raw) => rows_from_text(&raw),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(artist: Option<&str>) -> Song {
        Song {
            path: PathBuf::from("/music/晴天.mp3"),
            title: "晴天".into(),
            artist: artist.map(str::to_string),
        }
    }

    #[test]
    fn song_display_puts_the_artist_after_a_dash() {
        assert_eq!(song(Some("周杰伦")).display(), "《晴天》—— 周杰伦");
        assert_eq!(song(None).display(), "《晴天》");
        assert_eq!(
            song(Some("   ")).display(),
            "《晴天》",
            "只有空白的艺术家不该留下破折号"
        );
    }

    #[test]
    fn lyric_path_sits_next_to_the_audio() {
        assert_eq!(
            song(None).lyric_path(),
            PathBuf::from("/music/晴天.lrc"),
            "歌词应写在音频同目录的同名文件里"
        );
    }

    #[test]
    fn rows_from_text_keeps_time_and_content() {
        let rows = rows_from_text("[00:12.50]第一句\n[01:05.00]第二句\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].time, "00:12.50");
        assert_eq!(rows[0].text, "第一句");
        assert_eq!(rows[1].time, "01:05.00");
    }

    #[test]
    fn rows_from_text_drops_text_without_timestamps() {
        assert!(rows_from_text("只是一段没有时间的文本").is_empty());
        assert!(rows_from_text("").is_empty());
    }

    #[test]
    fn a_missing_lyric_file_reads_as_empty() {
        assert!(
            read_rows(Path::new("/definitely/not/here.lrc")).is_empty(),
            "没有歌词文件时应是空表格，不该报错"
        );
    }

    /// 目标目录（测试可写的工作区 `target/`）。
    fn fixture_dir() -> PathBuf {
        let dir = std::env::current_dir().expect("工作目录").join("target");
        std::fs::create_dir_all(&dir).expect("建 target");
        dir
    }

    #[gpui_kit::test]
    fn a_song_without_lyrics_asks_where_they_should_come_from(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.update(crate::ui::theme::init);
        let song = Song {
            path: PathBuf::from("/definitely/not/here.mp3"),
            title: "晴天".into(),
            artist: Some("周杰伦".into()),
        };
        let (view, cx) = cx.add_window_view(|window, cx| {
            LyricsEditorView::new(window, cx, Some(song))
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        assert!(
            cx.update(|_, cx| view.read(cx).notice.is_open()),
            "没有歌词时应先问用户歌词从哪来"
        );
        let popup = cx
            .debug_bounds("lyrics-notice-popup")
            .expect("提示框应该真的画出来");
        assert!(
            popup.size.width > px(200.) && popup.size.height > px(80.),
            "提示框尺寸不合常理：{popup:?}"
        );

        // 选「为这首歌新建歌词」：提示框关上，留一张空白表格等用户写。
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.start_new_lyrics(window, cx));
            view.update(cx, |view, cx| view.notice.close(window, cx));
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("lyrics-notice-popup").is_none(),
            "选完按钮提示框应关闭"
        );
        let (rows, bound) = cx.update(|_, cx| {
            let view = view.read(cx);
            (view.rows.len(), view.song.is_some())
        });
        assert_eq!(rows, 1, "新建歌词应该只有一行空白");
        assert!(bound, "编辑器应记住这首歌，保存时才知道写回哪");
    }

    #[gpui_kit::test]
    fn a_song_with_lyrics_loads_them_without_a_dialog(cx: &mut gpui_kit::TestAppContext) {
        cx.update(crate::ui::theme::init);
        let audio = fixture_dir().join("lyrics-editor-bound.mp3");
        std::fs::write(
            audio.with_extension("lrc"),
            "[00:01.00]第一句\n[00:02.00]第二句\n",
        )
        .expect("写测试 .lrc");

        let song = Song {
            path: audio,
            title: "已带歌词".into(),
            artist: None,
        };
        let (view, cx) = cx.add_window_view(|window, cx| {
            LyricsEditorView::new(window, cx, Some(song))
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let (rows, open) = cx.update(|_, cx| {
            let view = view.read(cx);
            (view.rows.len(), view.notice.is_open())
        });
        assert_eq!(rows, 2, "已有歌词应直接铺进表格");
        assert!(!open, "有歌词就不该再问一遍");
    }
}
