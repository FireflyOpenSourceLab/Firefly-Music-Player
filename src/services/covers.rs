//! 封面缓存。
//!
//! 音乐库卡片和浮动播放栏都要显示内嵌封面，但一张图动辄几百 KB：
//! 不能在渲染路径里同步读盘，也不能一次把整个音乐库的图都读进内存。
//!
//! 做法是按需加载 —— 渲染时只查缓存，命中就画，没命中就排进队列并先画占位，
//! 由**唯一一个**后台任务按批次读文件，读回来再重绘。缓存区分三种状态，
//! 所以「查过但没有封面」的歌曲不会每帧重新读一遍。

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::services::tags;

use gpui_kit::{App, Global, Image, ImageFormat, ImageSource};

/// 一批读多少张就回去重绘一次。
const BATCH: usize = 8;
/// 两次重绘之间至少隔这么久：几千首歌时逐张重绘会把界面拖住。
const REPAINT_INTERVAL: Duration = Duration::from_millis(120);
/// 已缓存的图片总字节上限。超过之后只画占位，不再往内存里堆图
/// （无损大图有几 MB 一张，几千首全读进来会吃掉几个 GB）。
const BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// 一首歌的封面状态。
enum Entry {
    /// 排进队列了，还没读回来。
    Pending,
    /// 读完了：`Some` 是有封面，`None` 是确认没有（别再读了）。
    Done(Option<ImageSource>),
}

/// 封面缓存的全局状态。
#[derive(Default)]
pub struct Covers {
    entries: HashMap<PathBuf, Entry>,
    queue: VecDeque<PathBuf>,
    /// 后台任务是否在跑：只允许一个，避免几千条请求各开一个任务。
    working: bool,
    /// 已经缓存住的图片字节数，用来守住 [`BYTE_BUDGET`]。
    bytes: usize,
}

impl Global for Covers {}

/// 装好封面缓存；应用启动时调一次。
pub fn init(cx: &mut App) {
    cx.set_global(Covers::default());
}

/// 取已经解好的封面（渲染路径专用：命中不了就返回 `None`，不会等）。
pub fn get(cx: &App, path: &Path) -> Option<ImageSource> {
    match cx.try_global::<Covers>()?.entries.get(path)? {
        Entry::Done(Some(source)) => Some(source.clone()),
        _ => None,
    }
}

/// 渲染时用这一个就够：有封面直接给，没查过就顺手排进后台队列。
pub fn thumb(cx: &mut App, path: &Path) -> Option<ImageSource> {
    request(cx, path);
    get(cx, path)
}

/// 请后台读某一首的封面。重复调用只排一次，渲染里可以放心每帧调。
pub fn request(cx: &mut App, path: &Path) {
    if cx.try_global::<Covers>().is_none() {
        return;
    }
    let start = {
        let covers = cx.global_mut::<Covers>();
        if covers.entries.contains_key(path) {
            return;
        }
        covers
            .entries
            .insert(path.to_path_buf(), Entry::Pending);
        covers.queue.push_back(path.to_path_buf());
        // 已经有任务在跑的话，它下一批就会把这条捡走。
        let start = !covers.working;
        covers.working = true;
        start
    };
    if start {
        start_worker(cx);
    }
}

/// 忘掉某一首的缓存。封面刚被改写过时调用，下次渲染就会重新读。
pub fn forget(cx: &mut App, path: &Path) {
    if cx.try_global::<Covers>().is_none() {
        return;
    }
    let covers = cx.global_mut::<Covers>();
    // 已经占掉的字节额度不退：图可能还被渲染层引用着，退早了额度就不准了。
    covers.entries.remove(path);
    // 顺手把它从待读队列里摘掉，免得刚写进去的图又被旧内容读一遍。
    covers.queue.retain(|queued| queued != path);
}

/// 起后台任务：循环取一批、读一批、重绘一次，队列空了就退出。
fn start_worker(cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut painted = Instant::now() - REPAINT_INTERVAL;
        loop {
            let batch = cx.update(|cx| take_batch(cx));
            if batch.is_empty() {
                // 收尾：最后一批得画上，否则界面会停在占位上。
                cx.refresh();
                return;
            }

            let task = cx
                .background_executor()
                .spawn(async move { read_covers(batch) });
            let results = task.await;
            cx.update(|cx| store(cx, results));
            if painted.elapsed() >= REPAINT_INTERVAL {
                painted = Instant::now();
                cx.refresh();
            }
        }
    })
    .detach();
}

/// 从队列头取一批路径；队列空了顺手把「有任务在跑」的标记落下。
fn take_batch(cx: &mut App) -> Vec<PathBuf> {
    if cx.try_global::<Covers>().is_none() {
        return Vec::new();
    }
    let covers = cx.global_mut::<Covers>();
    let mut batch = Vec::with_capacity(BATCH);
    while batch.len() < BATCH {
        match covers.queue.pop_front() {
            Some(path) => batch.push(path),
            None => break,
        }
    }
    if batch.is_empty() {
        covers.working = false;
    }
    batch
}

/// 后台线程：一次读一批文件的内嵌封面（读不到就是 `None`，不算错误）。
fn read_covers(paths: Vec<PathBuf>) -> Vec<(PathBuf, Option<tags::Cover>)> {
    paths
        .into_iter()
        .map(|path| {
            let cover = tags::read_cover(&path);
            (path, cover)
        })
        .collect()
}

/// 把读回来的结果放进缓存。图片源在这里建：`ImageSource` 不能跨线程。
fn store(cx: &mut App, results: Vec<(PathBuf, Option<tags::Cover>)>) {
    if cx.try_global::<Covers>().is_none() {
        return;
    }
    let covers = cx.global_mut::<Covers>();
    for (path, cover) in results {
        let source = cover.and_then(|cover| {
            let size = cover.bytes.len();
            if covers.bytes.saturating_add(size) > BYTE_BUDGET {
                // 额度用完了：画占位，至少不会把内存吃干净。
                return None;
            }
            let source = source_for(&cover)?;
            covers.bytes += size;
            Some(source)
        });
        covers.entries.insert(path, Entry::Done(source));
    }
}

/// 把原始图片字节变成 gpui 能画的图片源。
///
/// MIME 不可信（有的是空串、有的写错），所以先按内容猜一遍，猜不出来再信标签。
fn source_for(cover: &tags::Cover) -> Option<ImageSource> {
    let mime = tags::sniff_cover_mime(&cover.bytes).unwrap_or(cover.mime.as_str());
    let format = ImageFormat::from_mime_type(mime)?;
    Some(ImageSource::from(Arc::new(Image::from_bytes(
        format,
        cover.bytes.clone(),
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::tags::TINY_PNG;

    /// 按内容的 MIME 优先于标签里写的（标签经常是空的或错的）。
    #[test]
    fn the_cover_format_follows_the_content_not_the_label() {
        let cover = tags::Cover {
            bytes: TINY_PNG.to_vec(),
            mime: String::new(),
        };
        assert!(
            source_for(&cover).is_some(),
            "标签没写 MIME 也要能从内容认出 PNG"
        );

        let wrong = tags::Cover {
            bytes: TINY_PNG.to_vec(),
            mime: "image/jpeg".into(),
        };
        assert!(
            source_for(&wrong).is_some(),
            "标签写错了也不该挡住显示"
        );

        let junk = tags::Cover {
            bytes: b"not an image".to_vec(),
            mime: "image/png".into(),
        };
        assert!(source_for(&junk).is_none(), "解不了的字节不要送给渲染层");
    }
}
