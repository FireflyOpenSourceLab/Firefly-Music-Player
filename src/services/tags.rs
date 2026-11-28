//! 音频文件标签读写（ID3v2 / Vorbis Comments / MP4 ilst / APE …）。
//!
//! 元数据弹窗点「保存」时走 [`write`]，把 8 个字段真正写进文件本身；扫描音乐库
//! 时用 [`read`] 把文件里已有的标题、艺术家读出来，覆盖「文件名推断」的结果。
//!
//! 标签格式由 lofty 按容器自动选择，上层只认 [`Tags`]，不关心是 ID3 还是 ilst。

use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile as _, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;
use lofty::tag::Tag;

/// 一次编辑对应的八个字段。
///
/// 全部用字符串保存：表单里就是文本框，空字符串表示「这个字段留空」，
/// 写入时会把对应的标签项删掉，而不是写一个空值。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub year: String,
    pub genre: String,
    pub track_no: String,
    pub disc_no: String,
}

impl Tags {
    /// 八个字段是否都为空。
    pub fn is_empty(&self) -> bool {
        self == &Tags::default()
    }
}

/// 读出文件里已有的标签；没有标签时返回全空（不是错误）。
pub fn read(path: &Path) -> Result<Tags, String> {
    let tagged = Probe::open(path)
        .map_err(|error| format!("打不开文件：{error}"))?
        // 扫描只需要标签，不解析音频属性，省掉大文件的开销。
        .options(ParseOptions::new().read_properties(false))
        .read()
        .map_err(|error| format!("读不出文件里的标签：{error}"))?;

    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(Tags::default());
    };

    Ok(Tags {
        title: text(tag.title()),
        artist: text(tag.artist()),
        album: text(tag.album()),
        album_artist: tag
            .get_string(ItemKey::AlbumArtist)
            .map(str::to_string)
            .unwrap_or_default(),
        // ID3 写的是 TDRC、MP4 写的是 ©day，两边键名不一样，都试一遍。
        year: tag
            .get_string(ItemKey::RecordingDate)
            .or_else(|| tag.get_string(ItemKey::Year))
            .map(str::to_string)
            .unwrap_or_default(),
        genre: text(tag.genre()),
        track_no: number(tag.track()),
        disc_no: number(tag.disk()),
    })
}

/// 内嵌封面：原始图片字节 + 声明的 MIME。
///
/// MIME 决定 lofty 往 ID3 / Vorbis / MP4 里写什么类型标记，不能靠文件扩展名猜——
/// 用户可能把 `.png` 改成了 `.jpg`，所以由 [`sniff_cover_mime`] 按内容判断。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cover {
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// 保存时封面怎么处理。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoverChange<'a> {
    /// 用户没动封面：文件里原来的封面原样保留。
    #[default]
    Keep,
    /// 换成这一张。
    Set(&'a Cover),
    /// 把文件里的封面删掉。
    Remove,
}

/// 读出文件里的正面封面（没有正面封面就退回第一张图）。
///
/// 弹窗打开时用它做预览；读不到（没有标签、没有图、格式坏了）都返回 `None`。
pub fn read_cover(path: &Path) -> Option<Cover> {
    let tagged = Probe::open(path)
        .ok()?
        // 只要标签里的图，不解析音频属性，省掉大文件的开销。
        .options(ParseOptions::new().read_properties(false))
        .read()
        .ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let picture = tag
        .get_picture_type(PictureType::CoverFront)
        .or_else(|| tag.pictures().first())?;

    Some(Cover {
        bytes: picture.data().to_vec(),
        mime: picture
            .mime_type()
            .map(|mime| mime.as_str().to_string())
            .unwrap_or_default(),
    })
}

/// 按内容猜封面的 MIME，只认能写进标签的几种格式。
///
/// 标签里存的是图片本体，播放器按 MIME 决定怎么解；猜错了（比如把 PNG 标成 JPEG）
/// 比没有封面还糟，所以认不出来的一律返回 `None`，由调用方拒绝。
pub fn sniff_cover_mime(bytes: &[u8]) -> Option<&'static str> {
    // PNG 的 8 字节签名。
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    // JPEG 以 FF D8 FF 开头。
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    // TIFF 的两种字节序写法。
    if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        return Some("image/tiff");
    }
    None
}

/// 把 [`Tags`] 写进文件；文件不存在、只读、格式不支持都会返回可读的原因。
pub fn write(path: &Path, tags: &Tags) -> Result<(), String> {
    write_all(path, tags, CoverChange::Keep)
}

/// 一次写回标签与封面。
///
/// 合并成一次保存，是为了让「字段 + 封面」要么都写进去、要么都不动：分两次写的话，
/// 第二次失败会留下半新半旧的文件，而用户看到的却是一句失败提示。
pub fn write_all(path: &Path, tags: &Tags, cover: CoverChange<'_>) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("文件不在了：{}", path.display()));
    }

    // 写入要连属性一起读，lofty 重写标签时需要知道音频数据的偏移。
    let mut tagged = Probe::open(path)
        .map_err(|error| format!("打不开文件：{error}"))?
        .read()
        .map_err(|error| format!("读不出文件结构：{error}"))?;

    // 老文件可能一个标签都没有（裸 WAV、刚转出来的 m4a），先按格式补一个空标签。
    if tagged.primary_tag().is_none() {
        let tag_type = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(tag_type));
    }
    let Some(tag) = tagged.primary_tag_mut() else {
        return Err("这个格式不支持写入标签".into());
    };

    apply(tag, tags);
    match cover {
        CoverChange::Keep => {}
        CoverChange::Set(cover) => set_cover(tag, cover)?,
        CoverChange::Remove => {
            // 玩家都按「第一张 / 正面封面」显示，只删正面会留下别的图，看着像没删掉。
            while !tag.pictures().is_empty() {
                tag.remove_picture(0);
            }
        }
    }

    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|error| format!("写入文件失败：{error}"))
}

/// 换掉正面封面：清掉旧的正面封面，再放新的（其它类型的图不动）。
fn set_cover(tag: &mut Tag, cover: &Cover) -> Result<(), String> {
    let mime = MimeType::from_str(&cover.mime);
    // `Unknown` 意味着容器不一定认这个 MIME，写进去可能读不出来。
    if matches!(mime, MimeType::Unknown(_)) {
        return Err(format!(
            "「{}」这种图片格式写不进封面，换成 PNG / JPEG / GIF / BMP / TIFF 再试",
            cover.mime
        ));
    }
    if cover.bytes.is_empty() {
        return Err("图片是空的，换一张再试".into());
    }

    tag.remove_picture_type(PictureType::CoverFront);
    tag.push_picture(
        Picture::unchecked(cover.bytes.clone())
            .pic_type(PictureType::CoverFront)
            .mime_type(mime)
            .into(),
    );
    Ok(())
}

/// 把歌词写进音频文件本身的内嵌歌词标签（ID3 `USLT` / Vorbis `LYRICS` / MP4 `©lyr`）。
///
/// 播放器读的是音频旁边的 `.lrc`，这里只是让文件「随身携带」歌词：拷走单个文件
/// 时歌词不丢。传空字符串等于删掉这条标签。
pub fn write_lyric(path: &Path, lyric: &str) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("文件不在了：{}", path.display()));
    }

    let mut tagged = Probe::open(path)
        .map_err(|error| format!("打不开文件：{error}"))?
        .read()
        .map_err(|error| format!("读不出文件结构：{error}"))?;

    if tagged.primary_tag().is_none() {
        let tag_type = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(tag_type));
    }
    let Some(tag) = tagged.primary_tag_mut() else {
        return Err("这个格式不支持写入内嵌歌词".into());
    };

    if lyric.trim().is_empty() {
        tag.remove_key(ItemKey::Lyrics);
        tag.remove_key(ItemKey::UnsyncLyrics);
    } else if !tag.insert_text(ItemKey::Lyrics, lyric.to_string())
        // ID3v2 没有同步歌词文本帧，只认 `USLT`（UnsyncLyrics）；MP4 / Vorbis 走 `Lyrics`。
        && !tag.insert_text(ItemKey::UnsyncLyrics, lyric.to_string())
    {
        return Err("这个格式写不进内嵌歌词".into());
    }

    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|error| format!("写入内嵌歌词失败：{error}"))
}

/// 把字段搬进标签；空字段对应删除。
fn apply(tag: &mut Tag, tags: &Tags) {
    set_text(tag, tags.title.trim(), ItemKey::TrackTitle, |tag, value| {
        tag.set_title(value)
    });
    set_text(
        tag,
        tags.artist.trim(),
        ItemKey::TrackArtist,
        |tag, value| tag.set_artist(value),
    );
    set_text(tag, tags.album.trim(), ItemKey::AlbumTitle, |tag, value| {
        tag.set_album(value)
    });
    set_text(tag, tags.genre.trim(), ItemKey::Genre, |tag, value| {
        tag.set_genre(value)
    });

    // 专辑艺术家和年份没有 Accessor，直接用 ItemKey。
    set_key(tag, tags.album_artist.trim(), ItemKey::AlbumArtist);
    set_year(tag, tags.year.trim());

    // 音轨号 / 碟号是数字，解析不出来就当没填。
    match parse_number(tags.track_no.trim()) {
        Some(number) => {
            tag.set_track(number);
        }
        None => tag.remove_key(ItemKey::TrackNumber),
    }
    match parse_number(tags.disc_no.trim()) {
        Some(number) => {
            tag.set_disk(number);
        }
        None => tag.remove_key(ItemKey::DiscNumber),
    }
}

/// 填了就走 setter（lofty 的 Accessor），清空就把对应的标签项删掉。
fn set_text<F>(tag: &mut Tag, value: &str, key: ItemKey, setter: F)
where
    F: FnOnce(&mut Tag, String),
{
    if value.is_empty() {
        tag.remove_key(key);
    } else {
        setter(tag, value.to_string());
    }
}

fn set_key(tag: &mut Tag, value: &str, key: ItemKey) {
    if value.is_empty() {
        tag.remove_key(key);
    } else {
        tag.insert_text(key, value.to_string());
    }
}

/// 年份可能带月日（`2003-07-31`），先整串写，读的时候原样还回来。
fn set_year(tag: &mut Tag, value: &str) {
    if value.is_empty() {
        tag.remove_key(ItemKey::RecordingDate);
        tag.remove_key(ItemKey::Year);
        return;
    }
    // 不同容器认的键不一样，写不进去就换一个再试。
    if !tag.insert_text(ItemKey::RecordingDate, value.to_string()) {
        tag.insert_text(ItemKey::Year, value.to_string());
    }
}

/// `Cow<str>` → `String`，顺手 trim。
fn text(value: Option<std::borrow::Cow<'_, str>>) -> String {
    value
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

/// 数字字段：0 视为没填（ID3 里 0 是「空」的约定写法）。
fn number(value: Option<u32>) -> String {
    value
        .filter(|value| *value > 0)
        .map(|value| value.to_string())
        .unwrap_or_default()
}

/// 从 `3` / `03` / `3/12` 里取出音轨号本体。
fn parse_number(value: &str) -> Option<u32> {
    let head = value.split('/').next()?.trim();
    head.parse::<u32>().ok().filter(|number| *number > 0)
}

/// 读出文件里记录的音频总时长（秒）；认不出就返回 `None`。
///
/// 播放器原来只看解码器（symphonia）报的时长，而 symphonia 的 `n_frames` 只对
/// 带 Xing/Info 头的 mp3、带 `stts` 的 mp4 之类格式有值：普通 CBR mp3、裸 ADTS
/// AAC 一律是 `None`，界面上于是永远显示 `--:--`、进度条也拖不动。
/// 这里按容器头另算一份（mp3 无 Xing 头时用比特率 × 数据长度估算），给解码器兜底。
///
/// 只读文件头，不解码音频，也不解析标签和内嵌封面，开销比 [`read`] 还小。
pub fn duration_seconds(path: &Path) -> Option<f32> {
    let tagged = Probe::open(path)
        .ok()?
        .options(
            ParseOptions::new()
                .read_properties(true)
                .read_tags(false)
                .read_cover_art(false),
        )
        .read()
        .ok()?;
    let seconds = tagged.properties().duration().as_secs_f32();
    (seconds.is_finite() && seconds > 0.).then_some(seconds)
}

/// 这个扩展名是否在标签读写的支持范围内（用于扫描时快速跳过非音频）。
pub fn is_supported_extension(extension: &str) -> bool {
    const SUPPORTED: [&str; 12] = [
        "mp3", "wav", "flac", "m4a", "m4b", "mp4", "aac", "ogg", "oga", "opus", "aiff", "aif",
    ];
    SUPPORTED.contains(&extension)
}

/// 测试用素材：1×1 透明 PNG，够解码器认出来，也给封面读写当输入。
#[cfg(test)]
pub(crate) const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xFC, 0xCF, 0xC0, 0x50,
    0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9, 0x8C, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// 测试用素材：44 字节头 + 一小段静音的 WAV，够 lofty 认容器就行。
#[cfg(test)]
pub(crate) fn write_test_wav(path: &Path) {
    let sample_rate: u32 = 8000;
    let samples: u32 = 800; // 0.1 秒
    let data_len = samples * 2;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend_from_slice(&vec![0u8; data_len as usize]);
    std::fs::write(path, bytes).expect("写测试 wav");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        let dir = std::env::current_dir().expect("工作目录").join("target");
        std::fs::create_dir_all(&dir).expect("建 target");
        let path = dir.join(name);
        write_test_wav(&path);
        path
    }

    #[test]
    fn writes_and_reads_every_field() {
        let path = fixture("tags-roundtrip.wav");
        let tags = Tags {
            title: "晴天".into(),
            artist: "周杰伦".into(),
            album: "叶惠美".into(),
            album_artist: "周杰伦".into(),
            year: "2003".into(),
            genre: "流行".into(),
            track_no: "3".into(),
            disc_no: "1".into(),
        };

        write(&path, &tags).expect("写标签");
        let read_back = read(&path).expect("读标签");
        assert_eq!(read_back, tags, "写进去的和读出来的应完全一致");
    }

    #[test]
    fn empty_field_removes_the_tag() {
        let path = fixture("tags-clear.wav");
        let mut tags = Tags {
            title: "先写一首".into(),
            artist: "无名".into(),
            ..Tags::default()
        };
        write(&path, &tags).expect("第一次写");

        tags.artist.clear();
        write(&path, &tags).expect("第二次写");
        let read_back = read(&path).expect("读标签");
        assert_eq!(read_back.title, "先写一首");
        assert!(read_back.artist.is_empty(), "清空的字段应被删除");
    }

    #[test]
    fn untagged_file_reads_as_empty() {
        let path = fixture("tags-empty.wav");
        let read_back = read(&path).expect("裸 wav 也能读");
        assert!(read_back.is_empty(), "没有标签时应是空的：{read_back:?}");
    }

    #[test]
    fn missing_file_reports_reason() {
        let error = write(Path::new("/definitely/not/here.mp3"), &Tags::default()).unwrap_err();
        assert!(error.contains("文件不在了"), "错误信息不对：{error}");
    }

    #[test]
    fn parses_track_numbers() {
        assert_eq!(parse_number("3"), Some(3));
        assert_eq!(parse_number("03"), Some(3));
        assert_eq!(parse_number("3/12"), Some(3));
        assert_eq!(parse_number("0"), None, "0 视为没填");
        assert_eq!(parse_number(""), None);
        assert_eq!(parse_number("A1"), None);
    }

    #[test]
    fn supported_extensions_cover_common_audio() {
        assert!(is_supported_extension("mp3"));
        assert!(is_supported_extension("flac"));
        assert!(!is_supported_extension("txt"));
    }

    /// 歌词编辑器保存时要写一份进文件标签，这里验一遍能写能读。
    #[test]
    fn writes_the_lyric_into_the_file() {
        let path = fixture("tags-lyric.wav");
        let lyric = "[00:01.00]第一句\n[00:02.00]第二句\n";
        write_lyric(&path, lyric).expect("写内嵌歌词");

        let tagged = Probe::open(&path).expect("重新打开").read().expect("读文件");
        let tag = tagged
            .primary_tag()
            .or_else(|| tagged.first_tag())
            .expect("写歌词时应补上标签");
        assert_eq!(
            tag.get_string(ItemKey::Lyrics)
                .or_else(|| tag.get_string(ItemKey::UnsyncLyrics)),
            Some(lyric),
            "内嵌歌词应原样读出"
        );

        // 传空串等于把这条标签删掉。
        write_lyric(&path, "").expect("清空内嵌歌词");
        let tagged = Probe::open(&path).expect("重新打开").read().expect("读文件");
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
        assert_eq!(
            tag.and_then(|tag| tag
                .get_string(ItemKey::Lyrics)
                .or_else(|| tag.get_string(ItemKey::UnsyncLyrics))),
            None,
            "清空后不该还有歌词标签"
        );
    }

    /// 封面能写能读，并且「只改字段」不会把封面弄丢。
    #[test]
    fn writes_and_reads_the_cover() {
        let path = fixture("tags-cover.wav");
        let cover = Cover {
            bytes: TINY_PNG.to_vec(),
            mime: "image/png".into(),
        };

        write_all(&path, &Tags::default(), CoverChange::Set(&cover)).expect("写封面");
        assert_eq!(read_cover(&path), Some(cover.clone()), "封面应原样读出");

        // 只改字段、没动封面：原来的封面必须留着。
        write(
            &path,
            &Tags {
                title: "有封面".into(),
                ..Tags::default()
            },
        )
        .expect("改字段");
        assert_eq!(read_cover(&path), Some(cover), "没动封面就不该丢");

        write_all(&path, &Tags::default(), CoverChange::Remove).expect("删封面");
        assert_eq!(read_cover(&path), None, "删掉后不该还有封面");
    }

    /// MIME 按内容猜，不能看扩展名（用户可能把 png 改成 jpg）。
    #[test]
    fn cover_mime_is_sniffed_from_content() {
        assert_eq!(sniff_cover_mime(TINY_PNG), Some("image/png"));
        assert_eq!(
            sniff_cover_mime(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_cover_mime(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff_cover_mime(b"BM......"), Some("image/bmp"));
        assert_eq!(sniff_cover_mime(b"II*\0....."), Some("image/tiff"));
        assert_eq!(sniff_cover_mime(b"RIFF....WAVE"), None, "音频不是图片");
        assert_eq!(sniff_cover_mime(b""), None);
    }

    /// 写不进标签的图片格式要当场拒绝，不能留下一张读不出来的图。
    #[test]
    fn unsupported_cover_mime_is_rejected() {
        let path = fixture("tags-cover-bad.wav");
        let cover = Cover {
            bytes: TINY_PNG.to_vec(),
            mime: "image/webp".into(),
        };

        let error = write_all(&path, &Tags::default(), CoverChange::Set(&cover)).unwrap_err();
        assert!(error.contains("写不进封面"), "错误信息不对：{error}");
        assert_eq!(read_cover(&path), None, "被拒绝时不该写进去半张图");
    }

    #[test]
    fn reads_duration_from_the_container() {        let path = fixture("tags-duration.wav");
        let seconds = duration_seconds(&path).expect("wav 头里就有总时长");
        assert!((seconds - 0.1).abs() < 0.02, "应约 0.1 秒：{seconds}");
    }

    #[test]
    fn duration_is_none_when_unreadable() {
        let dir = std::env::current_dir().expect("工作目录").join("target");
        std::fs::create_dir_all(&dir).expect("建 target");
        let path = dir.join("tags-not-audio.bin");
        std::fs::write(&path, b"not audio at all").expect("写测试文件");
        assert_eq!(duration_seconds(&path), None, "非音频文件应返回 None");
        assert_eq!(
            duration_seconds(Path::new("/definitely/not/here.mp3")),
            None,
            "文件不存在应返回 None"
        );
    }

    /// m4a 走的是 MP4 的 ilst 标签，跟 WAV 的 ID3 完全是两套，单独验一遍。
    #[cfg(target_os = "macos")]
    #[test]
    fn writes_and_reads_mp4_ilst() {
        let wav = fixture("tags-mp4-source.wav");
        let m4a = wav.with_extension("m4a");
        let converted = std::process::Command::new("afconvert")
            .args(["-f", "m4af", "-d", "aac"])
            .arg(&wav)
            .arg(&m4a)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !converted {
            // 没有 afconvert 的环境（非 macOS 开发机）直接跳过。
            return;
        }

        let tags = Tags {
            title: "花海".into(),
            artist: "周杰伦".into(),
            album: "魔杰座".into(),
            album_artist: "周杰伦".into(),
            year: "2008".into(),
            genre: "流行".into(),
            track_no: "4".into(),
            disc_no: "1".into(),
        };
        write(&m4a, &tags).expect("写 m4a 标签");
        assert_eq!(read(&m4a).expect("读 m4a 标签"), tags);

        // MP4 的封面存在 `covr` 里，跟 ID3 的 APIC 是两套，单独验一遍。
        let cover = Cover {
            bytes: TINY_PNG.to_vec(),
            mime: "image/png".into(),
        };
        write_all(&m4a, &tags, CoverChange::Set(&cover)).expect("写 m4a 封面");
        assert_eq!(read_cover(&m4a), Some(cover), "m4a 的封面应能读回来");
    }
}
