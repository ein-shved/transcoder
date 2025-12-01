use ez_ffmpeg::AVMediaType;
use ez_ffmpeg::codec::{self as ffcodec, CodecInfo};
use ez_ffmpeg::stream_info::{StreamInfo, find_all_stream_infos};
use ffmpeg_sys_next::AVCodecID;
use log::trace;
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::ops::Deref;
use std::path::Path;
use std::process::Command;
use std::sync::{LazyLock, Mutex, MutexGuard, RwLock, RwLockReadGuard};
use std::{fmt, io};

pub struct Transcoder<'a> {
    config: MutexGuard<'a, TranscoderConfig>,
}

#[derive(Debug, Deserialize, Serialize, Hash)]
pub struct RequiredAudio {
    language: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Hash)]
pub struct RequiredSubtitle {
    language: Option<String>,
}

type FileExtension = String;

#[derive(Debug, PartialEq, Deserialize, Serialize, Hash, Eq, PartialOrd, Ord)]
pub enum RequirementType {
    Video,
    Audio(RequiredAudio),
    Subtitle(RequiredSubtitle),
}

#[derive(Debug, PartialEq, Clone, Copy, Deserialize, Serialize, Hash, Eq, PartialOrd, Ord)]
pub enum RequirementLevel {
    All,
    AtLeastOne,
    WithOther,
    Ignore,
    Decline,
}

// Requirements are comparable to make them prioritized. One stream can be matched to several
// requirements. E.g. requirement with language=Some(rus) and level=All and another requirement
// with language=None and level=WithOther. In such case we should follow the most accurate
// requirement
#[derive(Debug, PartialEq, Deserialize, Serialize, Hash, Eq, PartialOrd, Ord)]
pub struct Requirement {
    what: RequirementType,
    level: RequirementLevel,
}

#[derive(Default, Deserialize, Serialize)]
pub struct TranscoderConfig {
    #[serde(deserialize_with = "deserialize_formats", alias = "supported-formats")]
    supported_formats: Vec<FileExtension>,
    #[serde(
        deserialize_with = "deserialize_codecs",
        serialize_with = "serialize_codecs",
        alias = "supported-codecs"
    )]
    supported_codecs: Vec<CodecInfoExtra>,
    #[serde(alias = "requirements")]
    required: BTreeSet<Requirement>,
}

static CONFIG: LazyLock<Mutex<TranscoderConfig>> =
    LazyLock::new(|| Mutex::new(TranscoderConfig::default()));

pub struct IndexedCodecs {
    encoders: HashMap<String, CodecInfoExtra>,
    decoders: HashMap<String, CodecInfoExtra>,
}

#[derive(Clone, Debug)]
pub struct CodecInfoExtra {
    codec: CodecInfo,
    encoder: bool,
    #[allow(dead_code)]
    decoder: bool,
}

static CODECS: LazyLock<RwLock<IndexedCodecs>> =
    LazyLock::new(|| RwLock::new(IndexedCodecs::new()));

#[derive(Debug)]
enum MediaFile<'a> {
    Input {
        input: Streams,
        config: &'a TranscoderConfig,
        path: &'a Path,
    },
    Other {
        path: &'a Path,
    },
}

#[derive(Debug)]
struct MediaFileTasks<'req> {
    config: &'req TranscoderConfig,
    tasks: Vec<RequirementTaks<'req>>,
}

#[derive(Debug)]
struct RequirementTaks<'req> {
    requirement: &'req Requirement,
    tasks: Vec<TranscodeTask>,
}

#[derive(Debug, Clone)]
struct TranscodeTask {
    stream_index: i32,
    action: TranscodeTaskType,
}

#[derive(PartialEq, Clone)]
enum TranscodeTaskType {
    Supported,
    Transcode(CodecInfoExtra),
}

trait Transcodable {
    fn transcode(self, dst: &Path) -> io::Result<()>;
}

trait GetAVCodec {
    fn get_avcodec(&self) -> Option<AVCodecID>;
}

trait GetAVMediaType {
    fn get_avmediatype(&self) -> AVMediaType;
}

trait GetIndex {
    fn get_index(&self) -> i32;
}

type Streams = Vec<StreamInfo>;

impl<'a> MediaFile<'a> {
    pub fn new(path: &'a Path, config: &'a TranscoderConfig) -> Self {
        if let Ok(streams) = find_all_stream_infos(path.as_os_str().to_str().unwrap()) {
            Self::Input {
                input: streams,
                config,
                path,
            }
        } else {
            Self::Other { path }
        }
    }
}

impl<'a> Transcoder<'a> {
    pub fn get() -> Self {
        Self {
            config: TranscoderConfig::get(),
        }
    }
    pub fn transcode(self, src: &Path, dst: &Path) -> io::Result<()> {
        MediaFile::new(src, &self.config).transcode(dst)
    }
}

impl Transcodable for MediaFile<'_> {
    fn transcode(self, dst: &Path) -> io::Result<()> {
        match self {
            Self::Input {
                input,
                config,
                path,
            } => (input, config, path).transcode(dst),
            Self::Other { path } => path.transcode(dst),
        }
    }
}

impl Transcodable for &Path {
    fn transcode(self, dst: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dst.parent().unwrap_or(Path::new("/")))?;
        std::os::unix::fs::symlink(self, dst)
    }
}

impl Transcodable for (Streams, &TranscoderConfig, &Path) {
    fn transcode(self, dst: &Path) -> io::Result<()> {
        let (streams, config, src) = self;
        let tasks = MediaFileTasks::new(&streams, config);
        if tasks.need_to_transcode(src) {
            (streams, tasks, src).transcode(dst)
        } else {
            src.transcode(dst)
        }
    }
}

impl TranscoderConfig {
    pub fn get<'a>() -> MutexGuard<'a, TranscoderConfig> {
        CONFIG.lock().unwrap()
    }
    pub fn set(config: TranscoderConfig) {
        let mut s = Self::get();
        *s = config;
    }
}

impl Transcodable for (Streams, MediaFileTasks<'_>, &Path) {
    fn transcode(self, dst: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dst.parent().unwrap_or(Path::new("/")))?;
        let (streams, tasks, src) = self;
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-y"); // Agree with all;
        cmd.arg("-i").arg(src); // add input;
        cmd.arg("-map").arg("0"); // start mapping for single input;
        streams.into_iter().fold(&mut cmd, |cmd, stream| {
            let task = tasks.find_task_for(&stream);
            // for each stream add its mapping job to command
            cmd.arg(&format!("-c:{}", stream.get_index())).arg(&task)
        });
        cmd.arg(dst); // Finally - set the output
        trace!("Calling ffmpeg: {cmd:#?}");
        let mut child = cmd.spawn()?;
        child.wait()?;
        Ok(())
    }
}

impl IndexedCodecs {
    pub fn new() -> Self {
        let mut encoders = HashMap::default();
        let mut decoders = HashMap::default();

        Self::put_codecs_to_map(&mut encoders, ffcodec::get_encoders());
        Self::put_codecs_to_map(&mut decoders, ffcodec::get_decoders());

        let encoders = encoders
            .into_iter()
            .map(|(n, codec)| {
                let codec = CodecInfoExtra {
                    codec,
                    encoder: true,
                    decoder: decoders.get(&n).is_some(),
                };
                (n, codec)
            })
            .collect::<HashMap<String, CodecInfoExtra>>();

        let decoders = decoders
            .into_iter()
            .map(|(n, codec)| {
                let codec = CodecInfoExtra {
                    codec,
                    decoder: true,
                    encoder: encoders.get(&n).is_some(),
                };
                (n, codec)
            })
            .collect();

        Self { encoders, decoders }
    }

    pub fn get<'a>() -> RwLockReadGuard<'a, Self> {
        CODECS.read().unwrap()
    }

    pub fn find_in(&self, name: &str) -> Option<&CodecInfoExtra> {
        let lower = name.to_lowercase();
        self.decoders
            .get(&lower)
            .or_else(|| self.encoders.get(&lower))
    }
    pub fn find_decoder_in(&self, name: &str) -> Option<&CodecInfoExtra> {
        let lower = name.to_lowercase();
        self.decoders.get(&lower)
    }
    pub fn find_encoder_in(&self, name: &str) -> Option<&CodecInfoExtra> {
        let lower = name.to_lowercase();
        self.encoders.get(&lower)
    }

    pub fn find(name: &str) -> Option<CodecInfoExtra> {
        Self::get().find_in(name).cloned()
    }
    pub fn find_encoder(name: &str) -> Option<CodecInfoExtra> {
        Self::get().find_encoder_in(name).cloned()
    }
    pub fn find_decoder(name: &str) -> Option<CodecInfoExtra> {
        Self::get().find_decoder_in(name).cloned()
    }

    fn put_codecs_to_map(map: &mut HashMap<String, CodecInfo>, codecs: Vec<CodecInfo>) {
        for codec in codecs.into_iter() {
            Self::put_codec_to_map(map, codec);
        }
    }

    fn put_codec_to_map(map: &mut HashMap<String, CodecInfo>, codec: CodecInfo) {
        let name = codec.codec_name.to_lowercase();
        let longname = codec.codec_long_name.to_lowercase();
        trace!("Indexing codec: {name} ({longname})");
        Self::put_codec_to_map_as(map, codec.clone(), name);
        Self::put_codec_to_map_as(map, codec, longname);
    }

    fn put_codec_to_map_as(map: &mut HashMap<String, CodecInfo>, codec: CodecInfo, name: String) {
        map.insert(name, codec);
    }
}

impl<'req> MediaFileTasks<'req> {
    pub fn new(streams: &Streams, config: &'req TranscoderConfig) -> Self {
        let mut tasks = vec![];

        for req in config.required.iter() {
            tasks.push(RequirementTaks::<'req>::new(config, streams, req));
        }
        Self { config, tasks }
    }

    pub fn need_to_transcode(&self, src: &Path) -> bool {
        if let Some(format) = get_format(src) {
            let mut format_supported = false;
            for supp in self.config.supported_formats.iter() {
                if *supp == format {
                    format_supported = true;
                }
            }
            if !format_supported {
                return true;
            }
        }
        for task in self.tasks.iter() {
            if task.need_to_transcode() {
                return true;
            }
        }
        false
    }
    fn find_task_for<'a>(&'a self, stream: &StreamInfo) -> TranscodeTaskType {
        let mut final_task = None;
        for task in self.tasks.iter() {
            for task in task.tasks.iter() {
                if task.stream_index == stream.get_index() {
                    final_task = Some(task);
                    break;
                }
            }
            if final_task.is_some() {
                break;
            }
        }
        final_task
            .map(|task| task.action.clone())
            .unwrap_or(TranscodeTaskType::Supported)
    }
}

impl<'req> RequirementTaks<'req> {
    pub fn new(
        config: &'req TranscoderConfig,
        streams: &Streams,
        requirement: &'req Requirement,
    ) -> Self {
        let mut tasks = Vec::<TranscodeTask>::default();
        for stream in streams.iter() {
            if *requirement == *stream {
                if let Some(task) = TranscodeTask::new(stream, config) {
                    tasks.push(task);
                }
            }
        }
        Self { requirement, tasks }
    }
    pub fn get_level(&self) -> RequirementLevel {
        self.requirement.level
    }
    pub fn need_to_transcode(&self) -> bool {
        let level = self.get_level();
        if self.tasks.is_empty()
            || level == RequirementLevel::WithOther
            || level == RequirementLevel::Ignore
            || level == RequirementLevel::Decline
        {
            return false;
        }
        for task in self.tasks.iter() {
            if task.need_to_transcode() {
                if level == RequirementLevel::All {
                    return true;
                }
            } else {
                if level == RequirementLevel::AtLeastOne {
                    return false;
                }
            }
        }
        return level == RequirementLevel::AtLeastOne;
    }
}

impl PartialEq<StreamInfo> for Requirement {
    fn eq(&self, stream: &StreamInfo) -> bool {
        self.what == *stream
    }
}

impl PartialEq<StreamInfo> for RequirementType {
    fn eq(&self, stream: &StreamInfo) -> bool {
        match self {
            Self::Video => match stream {
                StreamInfo::Video { .. } => true,
                _ => false,
            },
            Self::Audio(audio) => match stream {
                StreamInfo::Audio { metadata, .. } => {
                    let media_lang = metadata.get("language");
                    audio.language.is_none() || media_lang == audio.language.as_ref()
                }
                _ => false,
            },
            Self::Subtitle(subs) => match stream {
                StreamInfo::Subtitle { metadata, .. } => {
                    let media_lang = metadata.get("language");
                    subs.language.is_none() || media_lang == subs.language.as_ref()
                }
                _ => false,
            },
        }
    }
}

impl<'file> TranscodeTask {
    pub fn new(stream: &StreamInfo, config: &TranscoderConfig) -> Option<Self> {
        let mut action = None;
        for supp in config.supported_codecs.iter() {
            if let Some(codec) = stream.get_avcodec() {
                if codec == supp.codec_id {
                    action = Some(TranscodeTaskType::Supported);
                    break;
                } else if action.is_none() {
                    if supp.media_type == stream.get_avmediatype() &&
                        // We are able to transcode only in case our codec supports encoding
                        supp.encoder
                    {
                        action = Some(TranscodeTaskType::Transcode(supp.clone()))
                    }
                }
            }
        }
        action.map(|action| Self {
            stream_index: stream.get_index(),
            action,
        })
    }
    pub fn need_to_transcode(&self) -> bool {
        return self.action != TranscodeTaskType::Supported;
    }
}

impl fmt::Debug for TranscodeTaskType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Supported => write!(f, "Supported"),
            Self::Transcode(codec) => write!(f, "Transcode to {}", codec.codec_long_name),
        }
    }
}

impl AsRef<OsStr> for TranscodeTaskType {
    fn as_ref(&self) -> &OsStr {
        match self {
            TranscodeTaskType::Supported => OsStr::new("copy"),
            TranscodeTaskType::Transcode(codec) => OsStr::new(&codec.desc_name),
        }
    }
}

// Reverting ordering for Requirements with optional fields to make one with Some to be less then
// one with None field to make them greater priority.
//
// While we are implementing Ord manually - we have to implement other 3 traits manually to as it
// said in std::cmp documentation
impl Ord for RequiredAudio {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        prioritize(&self.language, &other.language)
    }
}

impl PartialOrd for RequiredAudio {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for RequiredAudio {
    fn eq(&self, other: &Self) -> bool {
        self.language == other.language
    }
}

impl Eq for RequiredAudio {}

impl Ord for RequiredSubtitle {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        prioritize(&self.language, &other.language)
    }
}

impl PartialOrd for RequiredSubtitle {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for RequiredSubtitle {
    fn eq(&self, other: &Self) -> bool {
        self.language == other.language
    }
}

impl Eq for RequiredSubtitle {}

impl fmt::Debug for TranscoderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", toml::to_string(self).map_err(|_| fmt::Error {})?)
    }
}

fn prioritize<T: Ord>(lh: &Option<T>, rh: &Option<T>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if let Some(lh) = lh {
        if let Some(rh) = rh {
            lh.cmp(rh)
        } else {
            Ordering::Less
        }
    } else {
        if rh.is_none() {
            Ordering::Equal
        } else {
            Ordering::Greater
        }
    }
}

fn deserialize_codecs<'de, D>(deserializer: D) -> Result<Vec<CodecInfoExtra>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let ids = Vec::<String>::deserialize(deserializer)?;
    let mut res = Vec::<CodecInfoExtra>::new();
    res.reserve(ids.len());
    for id_str in ids.into_iter() {
        let codec = IndexedCodecs::find(&id_str)
            .ok_or_else(|| serde::de::Error::custom(&format!("Unknown codec {id_str}")))?;
        res.push(codec);
    }
    Ok(res)
}

fn serialize_codecs<S>(codecs: &Vec<CodecInfoExtra>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut sec = serializer.serialize_seq(Some(codecs.len()))?;
    for c in codecs {
        sec.serialize_element(&format!("{}", c.codec_long_name))?;
    }
    sec.end()
}

fn deserialize_formats<'de, D>(deserializer: D) -> Result<Vec<FileExtension>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Vec::<FileExtension>::deserialize(deserializer)?
        .into_iter()
        .map(|s| s.to_lowercase())
        .collect())
}

fn get_format(path: &Path) -> Option<String> {
    if let Some(s) = path.extension() {
        s.to_str().map(str::to_lowercase)
    } else {
        None
    }
}

impl Deref for CodecInfoExtra {
    type Target = CodecInfo;

    fn deref(&self) -> &Self::Target {
        &self.codec
    }
}

impl PartialEq for CodecInfoExtra {
    fn eq(&self, other: &Self) -> bool {
        self.codec_name == other.codec_name
    }
}

impl GetAVCodec for CodecInfo {
    fn get_avcodec(&self) -> Option<AVCodecID> {
        Some(self.codec_id)
    }
}
impl GetAVCodec for StreamInfo {
    fn get_avcodec(&self) -> Option<AVCodecID> {
        match self {
            Self::Subtitle { codec_id, .. } => Some(*codec_id),
            Self::Video { codec_id, .. } => Some(*codec_id),
            Self::Audio { codec_id, .. } => Some(*codec_id),
            Self::Attachment { codec_id, .. } => Some(*codec_id),
            Self::Unknown { .. } => None,
            Self::Data { .. } => None,
        }
    }
}

impl GetAVMediaType for CodecInfo {
    fn get_avmediatype(&self) -> AVMediaType {
        self.media_type
    }
}

impl GetAVMediaType for StreamInfo {
    fn get_avmediatype(&self) -> AVMediaType {
        match self {
            Self::Subtitle { .. } => AVMediaType::AVMEDIA_TYPE_SUBTITLE,
            Self::Video { .. } => AVMediaType::AVMEDIA_TYPE_VIDEO,
            Self::Audio { .. } => AVMediaType::AVMEDIA_TYPE_AUDIO,
            Self::Attachment { .. } => AVMediaType::AVMEDIA_TYPE_ATTACHMENT,
            Self::Unknown { .. } => AVMediaType::AVMEDIA_TYPE_UNKNOWN,
            Self::Data { .. } => AVMediaType::AVMEDIA_TYPE_DATA,
        }
    }
}

impl GetIndex for StreamInfo {
    fn get_index(&self) -> i32 {
        *match self {
            Self::Subtitle { index, .. } => index,
            Self::Video { index, .. } => index,
            Self::Audio { index, .. } => index,
            Self::Attachment { index, .. } => index,
            Self::Unknown { index, .. } => index,
            Self::Data { index, .. } => index,
        }
    }
}
