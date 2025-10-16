use ffmpeg_next::codec::traits::Encoder;
use ffmpeg_next::format::context;
use ffmpeg_next::{self as ffmpeg, codec, encoder, media, Codec, Packet, Stream};
use log::{debug, info, trace};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};
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
    supported_codecs: Vec<Codec>,
    #[serde(alias = "requirements")]
    required: BTreeSet<Requirement>,
}

static CONFIG: LazyLock<Mutex<TranscoderConfig>> =
    LazyLock::new(|| Mutex::new(TranscoderConfig::default()));

enum MediaFile<'a> {
    Input {
        input: context::Input,
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
    stream_index: usize,
    action: TranscodeTaskType,
}

#[derive(PartialEq, Clone)]
enum TranscodeTaskType {
    Supported,
    Transcode(Codec),
}

struct ProgramTask<'a> {
    source: StreamCodec<'a>
    action: TranscodeTaskType,
    encoder: ffmpeg::encoder::Encoder,
}

struct StreamCodec<'a> {
    stream: Stream<'a>,
    packet: Packet,
    codec: Option<Codec>,
}

type Streams<'a> = Vec<StreamCodec<'a>>;
type Program<'a> = Vec<ProgramTask<'a>>;

impl<'a> MediaFile<'a> {
    pub fn new(path: &'a Path) -> Self {
        let input = ffmpeg_next::format::input(path);
        if let Ok(input) = input {
            Self::Input { input, path }
        } else {
            Self::Other { path }
        }
    }

    pub fn path(&self) -> &Path {
        return match self {
            Self::Input { input: _, path } => path,
            Self::Other { path } => path,
        };
    }

    pub fn streams<'b>(&'b mut self) -> Streams<'b> {
        match self {
            MediaFile::Input { input, path: _ } => Self::make_streams(input),
            MediaFile::Other { path: _ } => vec![],
        }
    }

    pub fn make_streams<'b>(input: &'b mut context::Input) -> Streams<'b> {
        input.packets().map(StreamCodec::from).collect()
    }

    pub fn is_media(&self) -> bool {
        match self {
            Self::Input { input: _, path: _ } => true,
            _ => false,
        }
    }

    pub fn get_media(&self) -> Option<&context::Input> {
        match self {
            Self::Input { input, path: _ } => Some(input),
            _ => None,
        }
    }
}

impl fmt::Debug for MediaFile<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaFile::Input { input, path } => {
                writeln!(f, "File  {path:?}:")?;
                for (name, value) in input.metadata().iter() {
                    writeln!(f, "  {name}: {value}")?;
                }
                for (stream_index, stream) in input.streams().enumerate() {
                    let codec_parameters = stream.parameters();
                    let codec_id = codec_parameters.id();

                    writeln!(f, "  Stream {}:", stream_index)?;

                    for (name, value) in stream.metadata().iter() {
                        writeln!(f, "    {name}: {value}")?;
                    }

                    writeln!(f, "      Codec ID: {:?}", codec_id)?;

                    if let Some(codec) = find_codec(codec_id) {
                        writeln!(f, "      Codec Name: {}", codec.name())?;
                        writeln!(f, "      Codec Long Name: {}", codec.description())?;
                        writeln!(f, "      Codec Type: {:?}", codec.medium())?;
                    } else {
                        writeln!(f, "      Codec not found for ID: {:?}", codec_id)?;
                    }
                }
            }
            MediaFile::Other { path } => write!(f, "Unsupported {path:?}")?,
        }
        Ok(())
    }
}

impl<'a> Transcoder<'a> {
    pub fn get() -> Self {
        Self {
            config: TranscoderConfig::get(),
        }
    }
    pub fn transcode(self, src: &Path, dst: &Path) -> io::Result<()> {
        let mut file = MediaFile::new(src);
        debug!("{file:#?}");
        let file = file.streams();
        let parent = dst.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid destination without parent",
            )
        })?;

        trace!("{src:#?}");
        std::fs::create_dir_all(parent)?;
        if !self.transcode_media(file, src, dst)? {
            info!("Placing symlink to {src:?}");
            std::os::unix::fs::symlink(src, dst)?;
        }
        Ok(())
    }

    pub fn make_dst(&self, dst: &Path) -> PathBuf {
        dst.with_extension(&self.config.supported_formats[0])
    }

    fn transcode_media(self, streams: Streams, src: &Path, dst: &Path) -> io::Result<bool> {
        let tasks = MediaFileTasks::new(&streams, &self.config);
        trace!("Transcoding tasks: {tasks:?}");
        Ok(if tasks.need_to_transcode(src) {
            info!("Performing transcoding for {:?}", src);
            let dst = self.make_dst(dst);
            let dst = ffmpeg::format::output(&dst)?;
            tasks.transcode(streams, dst)?;
            true
        } else {
            false
        })
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
    fn make_program<'a>(&self, src: Streams<'a>, dst: &mut context::Output) -> Vec<ProgramTask<'a>> {
        src.into_iter().filter_map(|stream| {
            self.make_task_for(stream, dst)
        }).collect()
    }
    fn make_task_for<'a>(&self, stream: StreamCodec<'a>, dst: &mut context::Output) -> Option<ProgramTask<'a>> {
        let task = self.find_task_for(&stream);
        let codec = if let Some(task) = task {
            match task.action {
                TranscodeTaskType::Transcode(ref codec) => Some(codec),
                TranscodeTaskType::Supported => stream.codec.as_ref(),
            }
        } else {
            stream.codec.as_ref()
        }?;
        let mut out_stream = dst.add_stream(*codec).ok()?;
        out_stream.set_parameters(stream.stream.parameters());
        let mut encoder = codec.encoder()?.video().ok()?;

        todo!()
    }
    fn find_task_for<'a>(&'a self, stream: &StreamCodec) -> Option<&'a TranscodeTask> {
            let mut final_task = None;
            for task in self.tasks.iter() {
                for task in task.tasks.iter() {
                    if task.stream_index == stream.stream.index() {
                        final_task = Some(task);
                        break;
                    }
                }
                if final_task.is_some() {
                    break;
                }
            }
            final_task
    }
    pub fn transcode(&self, src: Streams, dst: context::Output) -> io::Result<()> {
        dst.write_header();
        for StreamCodec {
            stream,
            packet,
            codec,
        } in src.into_iter()
        {
            let mut final_task = None;
            for task in self.tasks.iter() {
                for task in task.tasks.iter() {
                    if task.stream_index == stream.index() {
                        final_task = Some(task.clone());
                        break;
                    }
                }
                if final_task.is_some() {
                    break;
                }
            }
            if let Some(task) = final_task {
                task.transcode(stream, packet, codec);
            } else {
                let dst = dst.add_stream(codec);
            }
        }
        Ok(())
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

impl PartialEq<StreamCodec<'_>> for Requirement {
    fn eq(&self, stream: &StreamCodec<'_>) -> bool {
        self.what == *stream
    }
}

impl PartialEq<StreamCodec<'_>> for RequirementType {
    fn eq(&self, stream: &StreamCodec<'_>) -> bool {
        use media::Type;
        if let Some(ref codec) = stream.codec {
            let media = codec.medium();
            let media_meta = stream.stream.metadata();
            let media_lang = media_meta.get("language");
            match self {
                Self::Video => media == Type::Video,
                Self::Audio(audio) => {
                    media == Type::Audio
                        && (audio.language.is_none() || media_lang == audio.language.as_deref())
                }
                Self::Subtitle(subs) => {
                    media == Type::Subtitle
                        && (subs.language.is_none() || media_lang == subs.language.as_deref())
                }
            }
        } else {
            false
        }
    }
}

impl<'a> From<(Stream<'a>, Packet)> for StreamCodec<'a> {
    fn from(pair: (Stream<'a>, Packet)) -> Self {
        let (stream, packet) = pair;
        let codec_parameters = stream.parameters();
        let codec_id = codec_parameters.id();

        let codec = find_codec(codec_id);
        Self {
            stream,
            packet,
            codec,
        }
    }
}

impl<'file> TranscodeTask {
    pub fn new(stream: &'file StreamCodec<'file>, config: &TranscoderConfig) -> Option<Self> {
        let mut action = None;
        for supp in config.supported_codecs.iter() {
            if let Some(codec) = stream.codec {
                if codec == *supp {
                    action = Some(TranscodeTaskType::Supported);
                    break;
                } else if action.is_none() {
                    if supp.medium() == codec.medium() {
                        action = Some(TranscodeTaskType::Transcode(*supp))
                    }
                }
            }
        }
        action.map(|action| Self {
            action,
            stream_index: stream.stream.index(),
        })
    }
    pub fn need_to_transcode(&self) -> bool {
        return self.action != TranscodeTaskType::Supported;
    }
}

impl fmt::Debug for StreamCodec<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(codec) = self.codec {
            write!(
                f,
                "{{Stream {} {:?}: {:?}}}",
                self.stream.index(),
                codec.medium(),
                codec.id()
            )
        } else {
            write!(f, "{{Unsupported stream {}}}", self.stream.id())
        }
    }
}

impl fmt::Debug for TranscodeTaskType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Supported => write!(f, "Supported"),
            Self::Transcode(to) => write!(f, "Transcode to {:?}", to.id()),
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

fn find_codec(id: codec::Id) -> Option<Codec> {
    codec::decoder::find(id).or_else(|| codec::encoder::find(id))
}

fn find_codec_by_name(name: &str) -> Option<Codec> {
    codec::decoder::find_by_name(name).or_else(|| codec::encoder::find_by_name(name))
}

fn deserialize_codecs<'de, D>(deserializer: D) -> Result<Vec<Codec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let ids = Vec::<String>::deserialize(deserializer)?;
    let mut res = Vec::<Codec>::new();
    res.reserve(ids.len());
    for id_str in ids.into_iter() {
        let id_str = id_str.to_lowercase();
        let codec = find_codec_by_name(&id_str)
            .ok_or_else(|| serde::de::Error::custom(&format!("Unknown codec {id_str}")))?;
        res.push(codec);
    }
    Ok(res)
}

fn serialize_codecs<S>(codecs: &Vec<Codec>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut sec = serializer.serialize_seq(Some(codecs.len()))?;
    for c in codecs {
        sec.serialize_element(&format!("{:?}", c.id()))?;
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
