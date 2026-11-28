//! 真实音频输出：rodio 解码 + 系统默认输出设备，外加一份全局播放状态。
//!
//! 设计上有意把「状态」和「设备」分开：
//!
//! - [`NowPlaying`] 是纯数据快照（队列、位置、循环模式），界面只读它；
//! - [`Engine`] 持有 `OutputStream` 与当前 `Sink`，界面永远不直接碰。
//!
//! 播放位置靠一个常驻的 250ms 计时器从 `Sink::get_pos()` 同步回状态，
//! 一曲放完（`Sink::empty()`）时按循环模式自动续播；只有秒数变了才重绘，
//! 避免无谓地整窗刷新。

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::{App, Global};
use rodio::decoder::DecoderBuilder;
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};

use crate::ui::now_playing::{NowPlaying, RepeatMode, Track, load_lyrics};

/// 进度同步的节拍。
const TICK_MS: u64 = 250;
/// 默认音量（0..=1）。定在 50%：再往上就属于「可能伤听力」的区间，
/// 界面上要先过一道确认。
pub const DEFAULT_VOLUME: f32 = 0.5;

impl Global for NowPlaying {}

/// 一路输出设备。`OutputStream` 一旦释放声音就断了，所以必须一直留着。
struct Output {
    stream: OutputStream,
    /// 当前曲目的播放通道；换歌时整体替换，旧的随之丢弃（等于停止）。
    sink: RefCell<Option<Sink>>,
}

impl Global for Output {}

/// 设备打不开时也要有地方记原因，所以 `output` 是 `Option`。
struct Engine {
    output: Option<Output>,
    failure: Option<String>,
    /// 用户设定的音量（0..=1）。它是「设备级」的状态：换歌、重建 sink 都要沿用，
    /// 否则每切一首就跳回默认音量。
    volume: Cell<f32>,
    /// 计时器只起一次，避免多次 `init` 叠出多条循环。
    ticker_started: Cell<bool>,
}

impl Global for Engine {}

/// 应用启动时调一次：打开输出设备、初始化状态、拉起进度计时器。
pub fn init(cx: &mut App) {
    if cx.try_global::<Engine>().is_none() {
        let engine = match OutputStreamBuilder::open_default_stream() {
            Ok(stream) => Engine {
                output: Some(Output {
                    stream,
                    sink: RefCell::new(None),
                }),
                failure: None,
                volume: Cell::new(DEFAULT_VOLUME),
                ticker_started: Cell::new(false),
            },
            Err(error) => Engine {
                output: None,
                failure: Some(format!("打不开系统音频输出：{error}")),
                volume: Cell::new(DEFAULT_VOLUME),
                ticker_started: Cell::new(false),
            },
        };
        cx.set_global(engine);
    }
    if cx.try_global::<NowPlaying>().is_none() {
        cx.set_global(NowPlaying::default());
    }
    start_ticker(cx);
}

/// 读一份状态快照。界面同时要读队列和位置，clone 一份最省心。
pub fn state(cx: &App) -> NowPlaying {
    cx.try_global::<NowPlaying>().cloned().unwrap_or_default()
}

/// 浮动栏要画的内容：只拷需要的几个字段，不复制整个队列。
#[derive(Clone, Debug, Default)]
pub struct BarView {
    pub title: String,
    pub subtitle: String,
    pub lyric: String,
    pub elapsed: String,
    pub duration: String,
    /// 当前曲目的文件路径：封面缓存按它找图，没有曲目时是 `None`。
    pub path: Option<PathBuf>,
    /// 归一化播放进度（`0..=1`），浮动栏上沿的进度条按它画。
    pub progress: f32,
    /// 是否有当前曲目：没有时浮动栏不画进度条。
    pub loaded: bool,
    pub playing: bool,
    pub repeat: RepeatMode,
    pub error: Option<String>,
}

/// 读浮动栏要用的那一小份状态。
pub fn bar_view(cx: &App) -> BarView {
    let fallback = NowPlaying::default();
    let now = cx.try_global::<NowPlaying>().unwrap_or(&fallback);
    let track = now.current();
    BarView {
        title: track
            .map(|track| track.title.clone())
            .unwrap_or_else(|| "还没有在播放".into()),
        subtitle: track.map(|track| track.subtitle()).unwrap_or_default(),
        lyric: now.current_lyric(),
        elapsed: now.elapsed_label(),
        duration: now.duration_label(),
        path: track.map(|track| track.path.clone()),
        progress: now.progress(),
        loaded: track.is_some(),
        playing: now.playing,
        repeat: now.repeat,
        error: now.error.clone(),
    }
}

/// 列表里标「正在播的那首」只要路径和播放状态。
#[derive(Clone, Debug, Default)]
pub struct ListMark {
    pub path: Option<PathBuf>,
    pub playing: bool,
}

/// 读列表高亮要用的那一小份状态。
pub fn list_mark(cx: &App) -> ListMark {
    let fallback = NowPlaying::default();
    let now = cx.try_global::<NowPlaying>().unwrap_or(&fallback);
    ListMark {
        path: now.current().map(|track| track.path.clone()),
        playing: now.playing,
    }
}

/// 改全局状态：先取快照，改完写回。
fn update(cx: &mut App, change: impl FnOnce(&mut NowPlaying)) {
    let mut next = state(cx);
    change(&mut next);
    cx.set_global(next);
}

fn output(cx: &App) -> Option<&Output> {
    cx.try_global::<Engine>()?.output.as_ref()
}

/// 用整个队列开播，从 `index` 开始。
pub fn play(cx: &mut App, queue: Vec<Track>, index: usize) {
    if queue.is_empty() {
        return;
    }
    let index = index.min(queue.len() - 1);
    update(cx, |state| state.queue = queue);
    start_at(cx, index);
}

/// 播队列里的第 `index` 首。
pub fn play_index(cx: &mut App, index: usize) {
    start_at(cx, index);
}

/// 播放 / 暂停。没有队列时什么都不做。
pub fn toggle(cx: &mut App) {
    let snapshot = state(cx);
    if !snapshot.has_queue() {
        return;
    }

    if snapshot.playing {
        if let Some(output) = output(cx)
            && let Some(sink) = output.sink.borrow().as_ref()
        {
            sink.pause();
        }
        update(cx, |state| state.playing = false);
        cx.refresh_windows();
        return;
    }

    // 通道还在（只是暂停）就接着放，否则从头开这首。
    let mut resumed = false;
    if let Some(output) = output(cx)
        && let Some(sink) = output.sink.borrow().as_ref()
        && sink.is_paused()
        && !sink.empty()
    {
        sink.play();
        resumed = true;
    }
    if resumed {
        update(cx, |state| state.playing = true);
        cx.refresh_windows();
    } else {
        start_at(cx, snapshot.index);
    }
}

/// 下一首；到末尾回到第一首。
pub fn next(cx: &mut App) {
    let snapshot = state(cx);
    if !snapshot.has_queue() {
        return;
    }
    let index = (snapshot.index + 1) % snapshot.queue.len();
    start_at(cx, index);
}

/// 上一首：播过 3 秒以上先回到本曲开头，否则切上一首（播放器通行行为）。
pub fn prev(cx: &mut App) {
    let snapshot = state(cx);
    if !snapshot.has_queue() {
        return;
    }
    if snapshot.position > 3. {
        seek_seconds(cx, 0.);
        return;
    }
    let index = snapshot
        .index
        .checked_sub(1)
        .unwrap_or(snapshot.queue.len() - 1);
    start_at(cx, index);
}

/// 插到当前曲目之后播放。
pub fn insert_next(cx: &mut App, track: Track) {
    let snapshot = state(cx);
    if !snapshot.has_queue() {
        play(cx, vec![track], 0);
        return;
    }
    let at = snapshot.index + 1;
    update(cx, |state| state.queue.insert(at, track));
    cx.refresh_windows();
}

/// 按归一化进度跳转。
pub fn seek_progress(cx: &mut App, progress: f32) {
    let snapshot = state(cx);
    if snapshot.duration <= 0. {
        return;
    }
    seek_seconds(cx, progress.clamp(0., 1.) * snapshot.duration);
}

/// 跳到指定秒数。
pub fn seek_seconds(cx: &mut App, seconds: f32) {
    let seconds = seconds.max(0.);
    if let Some(output) = output(cx)
        && let Some(sink) = output.sink.borrow().as_ref()
    {
        // 源实在不支持 seek（少数编码）时别动位置：否则界面先跳过去，
        // 下一个 tick 又按 sink 的真实位置弹回来，看着像「点了没反应还闪一下」。
        if sink.try_seek(Duration::from_secs_f32(seconds)).is_err() {
            return;
        }
    }
    update(cx, |state| {
        state.position = if state.duration > 0. {
            seconds.min(state.duration)
        } else {
            seconds
        };
    });
    cx.refresh_windows();
}

/// 切换循环模式。
pub fn cycle_repeat(cx: &mut App) {
    update(cx, |state| state.repeat = state.repeat.cycled());
    cx.refresh_windows();
}

/// 当前音量（0..=1）。
pub fn volume(cx: &App) -> f32 {
    cx.try_global::<Engine>()
        .map(|engine| engine.volume.get())
        .unwrap_or(DEFAULT_VOLUME)
}

/// 调音量（0..=1）。值会记在引擎里，换歌时新 sink 沿用同一个值。
pub fn set_volume(cx: &mut App, volume: f32) {
    let volume = if volume.is_finite() {
        volume.clamp(0., 1.)
    } else {
        DEFAULT_VOLUME
    };
    if let Some(engine) = cx.try_global::<Engine>() {
        engine.volume.set(volume);
    }
    if let Some(output) = output(cx)
        && let Some(sink) = output.sink.borrow().as_ref()
    {
        sink.set_volume(volume);
    }
    // 音量浮层里的百分比是每帧读状态的，改完要让窗口重画。
    cx.refresh_windows();
}

/// 歌词文件刚被改过：正在放的就是这首歌时重读一遍 `.lrc`，浮动栏立刻跟上。
///
/// 换歌时才读歌词（见 [`start_at`]），所以编辑器里保存后要主动来叫一次，
/// 否则要等到下一首才看得到新歌词。
pub fn reload_lyrics_if_playing(cx: &mut App, path: &Path) {
    let now = state(cx);
    let Some(track) = now.current() else {
        return;
    };
    if track.path != path {
        return;
    }
    let lyrics = load_lyrics(path);
    update(cx, |state| state.lyrics = lyrics);
    cx.refresh_windows();
}

// ---- 内部实现 ----

/// 真正把队列里的第 `index` 首送去解码播放；失败时把原因写进状态。
fn start_at(cx: &mut App, index: usize) -> bool {
    let snapshot = state(cx);
    let Some(track) = snapshot.queue.get(index).cloned() else {
        return false;
    };
    let lyrics = load_lyrics(&track.path);

    let mut failure = None;
    let mut duration = 0.;
    // 音量是设备级的，换歌照样沿用用户调好的值。
    let volume = volume(cx);

    match output(cx) {
        Some(output) => match open_sink(output, &track.path, volume) {
            Ok((sink, seconds)) => {
                duration = seconds;
                // 赋新值会把旧的 Sink 丢掉，等于停掉上一首。
                *output.sink.borrow_mut() = Some(sink);
            }
            Err(error) => failure = Some(error),
        },
        None => {
            failure = cx
                .try_global::<Engine>()
                .and_then(|engine| engine.failure.clone())
                .or_else(|| Some("音频输出还没初始化".to_string()));
        }
    }

    let playing = failure.is_none();
    update(cx, |state| {
        state.index = index;
        state.position = 0.;
        state.duration = duration;
        state.lyrics = lyrics;
        state.playing = playing;
        state.error = failure;
    });
    cx.refresh_windows();
    playing
}

/// 开一路新通道并起播；`volume` 是当前用户音量。
fn open_sink(output: &Output, path: &Path, volume: f32) -> Result<(Sink, f32), String> {
    let file = File::open(path).map_err(|error| format!("打不开文件：{error}"))?;
    // 文件字节长度必须交给解码器：rodio 默认 `byte_len = None`、`is_seekable = false`，
    // 拿不到长度的流只能往前读，往回 seek 直接报 ForwardOnly——
    // 表现就是「只能往后点进度条，往前点没反应」。
    let byte_len = file
        .metadata()
        .map(|metadata| metadata.len())
        .map_err(|error| format!("读不到文件大小：{error}"))?;
    let decoder = decoder_for(BufReader::new(file), byte_len, path)
        .map_err(|error| format!("这个格式解不了（{error}）"))?;
    // decoder 交给 sink 之后就拿不到时长了，先取。
    let duration = pick_duration(
        decoder
            .total_duration()
            .map(|duration| duration.as_secs_f32()),
        // 容器头只在解码器报不出时才读：lofty 要连标签一起解析（含内嵌封面），
        // 没必要为每首歌都付这份开销。
        || crate::services::tags::duration_seconds(path),
    );
    let sink = Sink::connect_new(output.stream.mixer());
    sink.append(decoder);
    sink.set_volume(volume);
    Ok((sink, duration))
}

/// 总时长取哪个：解码器报的优先（最准），拿不到再退回容器头算的。
///
/// symphonia 只有拿到了 `n_frames` 才报得出时长，而没有 Xing/Info 头的 CBR mp3、
/// 裸 ADTS AAC 这类文件永远给 `None`；只信它的话界面上就只剩 `--:--`，
/// 进度条也没法按比例拖。
fn pick_duration(from_decoder: Option<f32>, from_header: impl FnOnce() -> Option<f32>) -> f32 {
    let usable = |seconds: Option<f32>| seconds.filter(|value| value.is_finite() && *value > 0.);
    usable(from_decoder)
        .or_else(|| usable(from_header()))
        .unwrap_or(0.)
}

/// 探测提示：mp4 家族给 MIME，其余给扩展名，认不出的交给内容探测。
enum ProbeHint {
    Extension(&'static str),
    Mime(&'static str),
}

/// 按扩展名选探测提示。
///
/// mp4 / m4a 家族不能靠扩展名猜：symphonia 的 atom 扫描会先 seek 再回退，
/// 走通用探测路径会抛 seek 错误（rodio 0.19 收到后直接 `unreachable!` 崩掉），
/// 必须显式把 MIME 传进去。
fn probe_hint(path: &Path) -> Option<ProbeHint> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    Some(match extension.as_str() {
        "m4a" | "m4b" | "m4p" | "m4r" | "mp4" | "mov" => {
            ProbeHint::Mime("audio/mp4")
        }
        "aac" => ProbeHint::Extension("aac"),
        "mp3" => ProbeHint::Extension("mp3"),
        "flac" => ProbeHint::Extension("flac"),
        "ogg" | "oga" => ProbeHint::Extension("ogg"),
        "wav" => ProbeHint::Extension("wav"),
        // 其余（aiff、caF 等）交给内容探测，能认的会认出来。
        _ => return None,
    })
}

/// 建解码器。
///
/// `byte_len` 一定要给：它同时打开 `is_seekable` 并让 symphonia 能算出随机访问的
/// 字节偏移量。缺了它，越过当前位置往回 seek 只会得到 `ForwardOnly` 错误。
fn decoder_for<R: std::io::Read + std::io::Seek + Send + Sync + 'static>(
    reader: R,
    byte_len: u64,
    path: &Path,
) -> Result<Decoder<R>, rodio::decoder::DecoderError> {
    let builder = DecoderBuilder::new()
        .with_data(reader)
        .with_byte_len(byte_len);

    match probe_hint(path) {
        Some(ProbeHint::Mime(mime)) => builder.with_mime_type(mime).build(),
        Some(ProbeHint::Extension(extension)) => builder.with_hint(extension).build(),
        None => builder.build(),
    }
}

/// 一曲播完后的去向；返回是否有状态变化。
fn advance(cx: &mut App) -> bool {
    let snapshot = state(cx);
    if !snapshot.has_queue() {
        return false;
    }
    match snapshot.repeat {
        RepeatMode::One => start_at(cx, snapshot.index),
        RepeatMode::All => start_at(cx, (snapshot.index + 1) % snapshot.queue.len()),
        RepeatMode::Off => match snapshot.next_index() {
            Some(index) => start_at(cx, index),
            None => {
                // 队列放完：停在最后一首末尾。
                update(cx, |state| {
                    state.playing = false;
                    state.position = state.duration;
                });
                cx.refresh_windows();
                true
            }
        },
    }
}

/// 计时器每拍做的事：同步位置、处理播完。
///
/// 没有队列、没有输出设备时都只是「这一拍没事干」，**不等于 App 已经关闭**，
/// 所以这里没有返回值——循环不能拿它判断该不该继续。
pub(crate) fn tick(cx: &mut App) {
    let snapshot = state(cx);
    if !snapshot.has_queue() {
        return;
    }
    let Some(output) = output(cx) else {
        return;
    };

    let (position, ended) = {
        let sink = output.sink.borrow();
        match sink.as_ref() {
            Some(sink) => (sink.get_pos().as_secs_f32(), sink.empty()),
            None => (0., true),
        }
    };

    if ended && snapshot.playing {
        advance(cx);
        return;
    }
    // 位置没到能看出来的差别就不重绘，省得整窗 4 次/秒地刷。
    if (position - snapshot.position).abs() < 0.2 {
        return;
    }
    update(cx, |state| state.position = position);
    cx.refresh_windows();
}

fn start_ticker(cx: &mut App) {
    let already_running = cx
        .try_global::<Engine>()
        .map(|engine| engine.ticker_started.replace(true))
        .unwrap_or(true);
    if already_running {
        return;
    }

    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(TICK_MS))
                .await;
            // 一直转下去，不看「这一拍有没有事干」：App 启动时队列是空的，
            // 这里如果按 `tick` 的结果决定 break，第一拍就会把计时器关掉，
            // 之后位置永远不同步（进度条卡在 0:00、播完也不会自动续播）。
            // 任务本身随 App 一起销毁，不需要自己判断退出。
            cx.update(|cx| tick(cx));
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 44 字节 WAV 头 + 单声道 16bit 正弦波，用来验证解码与设备可用。
    fn write_test_wav(path: &Path, seconds: f32) {
        const RATE: u32 = 44_100;
        let samples = (RATE as f32 * seconds) as u32;
        let data_len = samples * 2;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // 单声道
        bytes.extend_from_slice(&RATE.to_le_bytes());
        bytes.extend_from_slice(&(RATE * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for index in 0..samples {
            let t = index as f32 / RATE as f32;
            let value = (t * 440. * std::f32::consts::TAU).sin() * 0.2;
            bytes.extend_from_slice(&((value * i16::MAX as f32) as i16).to_le_bytes());
        }
        std::fs::write(path, bytes).expect("写测试音频");
    }

    /// 解码器必须支持往回 seek。
    ///
    /// rodio 的 `Decoder::new_mp3` 之类入口不会带文件长度，`is_seekable` 默认是 false，
    /// 这种流只能往前读：点进度条上「已经播过」的那一段会报 `ForwardOnly`，
    /// 界面上就是「只能往后点，往回点没反应」。
    #[test]
    fn decoder_seeks_backwards() {
        let path = std::env::current_dir()
            .expect("取工作目录")
            .join("target")
            .join(format!("seek-fixture-{}.wav", std::process::id()));
        write_test_wav(&path, 3.0);

        let file = File::open(&path).expect("打开测试音频");
        let byte_len = file.metadata().expect("读元数据").len();
        let mut decoder =
            decoder_for(BufReader::new(file), byte_len, &path).expect("应能解码 wav");

        // 先正常读掉一点，模拟「已经播到中段」。
        for _ in 0..44_100 {
            decoder.next();
        }

        decoder
            .try_seek(Duration::from_secs_f32(2.))
            .expect("往后跳应成功");
        // 关键：往回跳。
        decoder
            .try_seek(Duration::from_secs_f32(0.5))
            .expect("往回跳应成功（没有 byte_len 时这里会报 ForwardOnly）");

        // 跳完还能继续出声。
        assert!(decoder.next().is_some(), "回跳后应能继续解码");

        let _ = std::fs::remove_file(&path);
    }

    /// 探测提示要认对：mp4 家族必须给 MIME（内容探测会 seek 崩），
    /// 其余常见格式给扩展名，不认识的交给内容探测。
    #[test]
    fn probe_hints_cover_the_known_families() {
        let hint = |name: &str| probe_hint(Path::new(name)).map(|hint| match hint {
            ProbeHint::Extension(extension) => extension.to_string(),
            ProbeHint::Mime(mime) => mime.to_string(),
        });

        assert_eq!(hint("songs/测试.M4A").as_deref(), Some("audio/mp4"));
        assert_eq!(hint("a.mp3").as_deref(), Some("mp3"));
        assert_eq!(hint("a.flac").as_deref(), Some("flac"));
        assert_eq!(hint("a.ogg").as_deref(), Some("ogg"));
        assert_eq!(hint("a.aiff"), None, "不认识的扩展名交给内容探测");
        assert_eq!(hint("no-extension"), None);
    }

    /// 整条 sink 链路也要能回跳：真实播放时位置由音频线程回写，
    /// 只改界面状态是不够的。需要设备，默认不跑：`cargo test -- --ignored`。
    #[test]
    #[ignore = "需要系统音频输出设备"]
    fn sink_reports_a_backwards_seek() {
        let path = std::env::current_dir()
            .expect("取工作目录")
            .join("target")
            .join(format!("sink-seek-{}.wav", std::process::id()));
        write_test_wav(&path, 4.);
        let stream = match OutputStreamBuilder::open_default_stream() {
            Ok(stream) => stream,
            Err(_) => return,
        };
        let output = Output {
            stream,
            sink: RefCell::new(None),
        };
        let (sink, _duration) = open_sink(&output, &path, 0.).expect("应能解码并起播");

        sink.try_seek(Duration::from_secs_f32(2.5)).expect("往后跳");
        sink.try_seek(Duration::from_secs_f32(0.5)).expect("往回跳");
        // 音频线程每 5ms 用源的真实位置覆盖一次，等它跑过几拍再看。
        std::thread::sleep(Duration::from_millis(200));

        let position = sink.get_pos().as_secs_f32();
        assert!(
            (position - 0.5).abs() < 0.4,
            "回跳到 0.5 秒后应停在那儿，实际 {position}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// 需要真实音频设备，默认不跑：`cargo test -- --ignored`。
    #[test]
    #[ignore = "需要系统音频输出设备"]
    fn decodes_and_plays_a_generated_wav() {
        let path = std::env::current_dir()
            .expect("取工作目录")
            .join("target")
            .join(format!("playback-fixture-{}.wav", std::process::id()));
        write_test_wav(&path, 0.3);

        let stream = OutputStreamBuilder::open_default_stream().expect("应有默认输出设备");
        let output = Output {
            stream,
            sink: RefCell::new(None),
        };
        let (sink, duration) = open_sink(&output, &path, DEFAULT_VOLUME).expect("应能解码并起播");
        assert!(
            (duration - 0.3).abs() < 0.05,
            "时长应约为 0.3 秒：{duration}"
        );
        sink.set_volume(0.);
        sink.sleep_until_end();
        assert!(sink.empty(), "播完队列应为空");
    }

    /// m4a（AAC）是最容易「点了没反应」的格式：用系统自带 afconvert 造一个再解，
    /// 验证 `symphonia-isomp4` / `symphonia-aac` 两个特性真的生效。
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "需要系统音频输出设备"]
    fn decodes_m4a_made_by_afconvert() {
        let stream = match OutputStreamBuilder::open_default_stream() {
            Ok(stream) => stream,
            Err(_) => return,
        };
        let dir = std::env::current_dir().expect("取工作目录").join("target");
        let wav = dir.join(format!("m4a-fixture-{}.wav", std::process::id()));
        let m4a = dir.join(format!("m4a-fixture-{}.m4a", std::process::id()));
        write_test_wav(&wav, 0.6);

        let converted = std::process::Command::new("afconvert")
            .args(["-f", "m4af", "-d", "aac"])
            .arg(&wav)
            .arg(&m4a)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !converted {
            // 没有 afconvert 就跳过，不算失败。
            return;
        }

        let output = Output {
            stream,
            sink: RefCell::new(None),
        };
        let (sink, duration) = open_sink(&output, &m4a, DEFAULT_VOLUME).expect("m4a 应该能解码");
        assert!(duration > 0.1, "m4a 时长应可读出：{duration}");
        sink.set_volume(0.);
        sink.sleep_until_end();
    }

    /// aiff 在 macOS 上到处都是（系统音效就是），音乐库里也列了它，确认能解。
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "需要系统音频输出设备"]
    fn decodes_a_system_aiff() {
        let sample = std::path::Path::new("/System/Library/Sounds/Ping.aiff");
        if !sample.exists() {
            return;
        }
        let stream = match OutputStreamBuilder::open_default_stream() {
            Ok(stream) => stream,
            Err(_) => return,
        };
        let output = Output {
            stream,
            sink: RefCell::new(None),
        };
        let (sink, duration) = open_sink(&output, sample, DEFAULT_VOLUME).expect("aiff 应该能解码");
        assert!(duration > 0., "aiff 时长应可读出：{duration}");
        sink.set_volume(0.);
        sink.sleep_until_end();
    }

    /// 解码器报不出时长时要退回容器头，否则整条进度就废了。
    #[test]
    fn duration_falls_back_to_container_header() {
        assert_eq!(
            pick_duration(Some(177.66), || Some(177.07)),
            177.66,
            "解码器有值就以它为准"
        );
        assert_eq!(
            pick_duration(None, || Some(147.07)),
            147.07,
            "mp3 没有 Xing 头时解码器是 None，应退回 lofty 算的时长"
        );
        assert_eq!(
            pick_duration(Some(0.), || Some(147.07)),
            147.07,
            "0 视为拿不到"
        );
        assert_eq!(
            pick_duration(Some(f32::NAN), || Some(1.5)),
            1.5,
            "NaN 视为拿不到"
        );
        assert_eq!(pick_duration(None, || None), 0., "两边都没有时保持未知");

        // 解码器有值时就该直接返回，不该再去读文件头（靠 panic 证明没被调用）。
        assert_eq!(pick_duration(Some(3.), || panic!("不该读容器头")), 3.);
    }

    /// 音量是设备级状态：换歌不能跳回默认值。
    #[gpui_kit::test]
    fn volume_survives_track_changes(cx: &mut gpui_kit::TestAppContext) {
        if OutputStreamBuilder::open_default_stream().is_err() {
            return;
        }
        let sample = Path::new("/System/Library/Sounds/Ping.aiff");
        if !sample.exists() {
            return;
        }

        cx.update(|cx| {
            init(cx);
            set_volume(cx, 0.25);
            assert_eq!(volume(cx), 0.25, "设完就该读到 0.25");

            play(cx, vec![Track::new(sample, "Ping")], 0);
            assert_eq!(volume(cx), 0.25, "起播后音量应沿用用户设定，而不是默认值");
            assert!(state(cx).playing, "应已起播：{:?}", state(cx).error);

            // 越界与非有限值都不该写进引擎。
            set_volume(cx, 2.);
            assert_eq!(volume(cx), 1.);
            set_volume(cx, f32::NAN);
            assert_eq!(volume(cx), DEFAULT_VOLUME, "NaN 应退回默认音量");
        });
    }

    /// 计时器不能在「还没播过任何东西」时把自己关掉。
    ///
    /// App 启动时队列是空的，曾经这里把 `tick` 返回的 false（= 这一拍没状态变化）
    /// 当成「App 已关闭」，第一拍就 `break`，此后位置永远不同步：
    /// 浮动栏卡在 `0:00`、进度条不动、放完也不会自动下一首。
    ///
    /// 这里用「队列里两首、假装正在放第一首、没有任何 sink」的假状态：存活下来的一拍
    /// 必然走到 `advance`，把当前曲目挪到第二首 —— 下标变了就说明计时器还在转。
    #[gpui_kit::test]
    fn ticker_survives_an_idle_start(cx: &mut gpui_kit::TestAppContext) {
        // 没有音频输出设备的环境（CI）直接跳过：没设备时 tick 会在取 output 时返回。
        if OutputStreamBuilder::open_default_stream().is_err() {
            return;
        }
        let sample = Path::new("/System/Library/Sounds/Ping.aiff");
        if !sample.exists() {
            return;
        }

        cx.update(|cx| init(cx));
        // 模拟启动后的一段空闲：这几拍里队列都是空的。
        cx.background_executor
            .advance_clock(Duration::from_millis(TICK_MS * 8));
        cx.run_until_parked();

        cx.update(|cx| {
            update(cx, |state| {
                state.queue = vec![Track::new(sample, "Ping"), Track::new(sample, "Ping 2")];
                state.index = 0;
                state.playing = true;
            });
        });

        // 再走过一拍：只有计时器还活着，才会把曲目推进到第二首。
        cx.background_executor
            .advance_clock(Duration::from_millis(TICK_MS * 2));
        cx.run_until_parked();

        cx.update(|cx| {
            let now = state(cx);
            assert_eq!(
                now.index, 1,
                "启动时空闲几拍后计时器仍应存活并推进曲目，实际 index={}（playing={}, error={:?}）",
                now.index, now.playing, now.error
            );
        });
    }

    #[test]
    fn missing_file_reports_reason() {
        let stream = match OutputStreamBuilder::open_default_stream() {
            Ok(stream) => stream,
            // 没有音频设备的环境（CI）跳过这条。
            Err(_) => return,
        };
        let output = Output {
            stream,
            sink: RefCell::new(None),
        };
        let error = match open_sink(
            &output,
            Path::new("/definitely/not/here.mp3"),
            DEFAULT_VOLUME,
        ) {
            Ok(_) => panic!("不存在的文件不该开出通道"),
            Err(error) => error,
        };
        assert!(error.contains("打不开文件"), "错误信息不对：{error}");
    }
}
