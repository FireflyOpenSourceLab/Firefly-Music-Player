//! 歌词编辑器窗口：从托盘菜单的「歌词编辑器」或音乐库右键「修改歌词」打开。
//!
//! 编辑器是独立窗口，和主窗口共用同一套主题；开过一次之后再次打开只会把
//! 已有窗口提到最前，不会重复开窗——带歌进来时顺手把那首歌交给窗口。

use gpui_kit::{AnyWindowHandle, App, AppContext as _, Global, WindowOptions, px, size};

use crate::ui::theme::follow_system;

use super::{Song, view::LyricsEditorView};

/// 窗口标题（也是系统标题栏上显示的文字）。
pub const TITLE: &str = "歌词编辑器";

/// 当前已打开的编辑器窗口；窗口被关掉后这个句柄就失效了。
struct OpenEditor(Option<AnyWindowHandle>);

impl Global for OpenEditor {}

/// 打开空白编辑器（托盘菜单用）；已经开着就只前置。
pub fn open(cx: &mut App) {
    open_editor(cx, None);
}

/// 为某首歌打开编辑器：已有歌词就载入表格，没有就在窗口里问歌词从哪来。
pub fn open_for_song(cx: &mut App, song: Song) {
    open_editor(cx, Some(song));
}

/// 开窗 / 复用窗口的唯一入口；`song` 为 `None` 时不动窗口里的内容。
fn open_editor(cx: &mut App, song: Option<Song>) {
    // 先看有没有还活着的窗口：`update` 在窗口已关闭时会返回 Err。
    if let Some(handle) = cx.try_global::<OpenEditor>().and_then(|open| open.0)
        && let Some(window) = handle.downcast::<LyricsEditorView>()
    {
        let target = song.clone();
        let alive = window.update(cx, |view, window, cx| {
            view.attach(target, window, cx);
            window.activate_window();
        });
        if alive.is_ok() {
            cx.activate(true);
            return;
        }
    }

    let options = WindowOptions {
        // 编辑歌词需要横向空间，给一个不小于主窗口高度的起始尺寸。
        window_min_size: Some(size(px(720.), px(520.))),
        ..Default::default()
    };

    let opened = cx.open_window(options, move |window, cx| {
        window.set_window_title(TITLE);
        // 跟随系统深浅色，和主窗口保持同一套主题。
        follow_system(window, cx).detach();
        cx.new(|cx| LyricsEditorView::new(window, cx, song))
    });

    match opened {
        Ok(handle) => cx.set_global(OpenEditor(Some(handle.into()))),
        Err(error) => eprintln!("打开歌词编辑器窗口失败: {error:?}"),
    }
}
