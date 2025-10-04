use ffmpeg_next::{self as ffmpeg, Codec, Stream, codec, media};
use log::{debug, info, trace};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::{fmt, io};

pub struct Transcoder<'a> {
    config: MutexGuard<'a, TranscoderConfig>,
}

#[derive(Debug, Deserialize, Hash)]
pub struct RequiredAudio {
    language: Option<String>,
}

#[derive(Debug, Deserialize, Hash)]
pub struct RequiredSubtitle {
    language: Option<String>,
}

type FileExtension = String;

#[derive(Debug, PartialEq, Deserialize, Hash, Eq, PartialOrd, Ord)]
pub enum RequirementType {
    Video,
    Audio(RequiredAudio),
    Subtitle(RequiredSubtitle),
}

#[derive(Debug, PartialEq, Clone, Copy, Deserialize, Hash, Eq, PartialOrd, Ord)]
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
#[derive(Debug, PartialEq, Deserialize, Hash, Eq, PartialOrd, Ord)]
pub struct Requirement {
    what: RequirementType,
    level: RequirementLevel,
}

#[derive(Default, Debug, Deserialize)]
pub struct TranscoderConfig {
    #[serde(deserialize_with = "deserialize_formats", alias = "supported-formats")]
    supported_formats: Vec<FileExtension>,
    #[serde(deserialize_with = "deserialize_codecs", alias = "supported-codecs")]
    supported_codecs: Vec<codec::Id>,
    #[serde(alias = "requirements")]
    required: BTreeSet<Requirement>,
}

static CONFIG: LazyLock<Mutex<TranscoderConfig>> =
    LazyLock::new(|| Mutex::new(TranscoderConfig::default()));

enum MediaFile<'a> {
    Input {
        input: ffmpeg::format::context::Input,
        path: &'a Path,
    },
    Other {
        path: &'a Path,
    },
}

#[derive(Debug)]
struct MediaFileTasks<'req, 'file> {
    file: &'file MediaFile<'file>,
    config: &'req TranscoderConfig,
    tasks: Vec<RequirementTaks<'req, 'file>>,
}

#[derive(Debug)]
struct RequirementTaks<'req, 'file> {
    requirement: &'req Requirement,
    tasks: Vec<TranscodeTask<'file>>,
}

#[derive(Debug, Clone)]
struct TranscodeTask<'file> {
    stream: &'file StreamCodec<'file>,
    action: TranscodeTaskType,
}

#[derive(PartialEq, Debug, Clone)]
enum TranscodeTaskType {
    Supported,
    Transcode(codec::Id),
}

struct StreamCodec<'a> {
    stream: Stream<'a>,
    codec: Option<Codec>,
}

type Streams<'a> = Vec<StreamCodec<'a>>;

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
    pub fn streams<'b>(&'b self) -> Streams<'b> {
        let mut res = vec![];
        match self {
            MediaFile::Input { input, path: _ } => {
                for stream in input.streams() {
                    res.push(StreamCodec::from(stream));
                }
            }
            MediaFile::Other { path: _ } => (),
        }
        res
    }
    pub fn is_media(&self) -> bool {
        match self {
            Self::Input { input: _, path: _ } => true,
            _ => false,
        }
    }

    pub fn get_format(&self) -> Option<String> {
        if let Some(s) = self.path().extension() {
            s.to_str().map(str::to_lowercase)
        } else {
            None
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
        let src = MediaFile::new(src);
        let streams = src.streams();
        let tasks = MediaFileTasks::new(&src, &streams, &self.config);
        trace!("{src:#?}");
        if src.is_media() {
            debug!("Transcoding tasks: {tasks:#?}");
        }
        if tasks.need_to_transcode() {
            info!("Performing transcoding for {:?}", src.path());
            let tasks = tasks.make_program(&streams);
            trace!("Will do: {tasks:?}");
            todo!();
        } else {
            info!("Placing symlink to {:?}", src.path());
            std::fs::create_dir_all(dst.parent().unwrap_or(Path::new("/")))?;
            std::os::unix::fs::symlink(src.path(), dst)?;
        }
        Ok(())
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

impl<'req, 'file> MediaFileTasks<'req, 'file> {
    pub fn new(
        file: &'file MediaFile<'file>,
        streams: &'file Streams<'file>,
        config: &'req TranscoderConfig,
    ) -> Self {
        let mut tasks = vec![];

        for req in config.required.iter() {
            tasks.push(RequirementTaks::<'req, 'file>::new(
                file, config, &streams, req,
            ));
        }
        Self { file, config, tasks }
    }

    pub fn need_to_transcode(&self) -> bool {
        if let Some(format) = self.file.get_format()
        {
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
    pub fn make_program(&self, streams: &'file Streams<'file>) -> Vec<TranscodeTask<'file>>
    {
        let mut res = Vec::new();
        for stream in streams.iter() {
            for task in self.tasks.iter() {
                let mut final_task = None;
                for task in task.tasks.iter() {
                    if task.stream.stream == stream.stream {
                        final_task = Some(task.clone());
                        break;
                    }
                }
                if let Some(task) = final_task {
                    res.push(task);
                    break
                }
            }

        }
        res
    }
}

impl<'req, 'file> RequirementTaks<'req, 'file> {
    pub fn new(
        _file: &'file MediaFile<'file>,
        config: &'req TranscoderConfig,
        streams: &'file Streams<'file>,
        requirement: &'req Requirement,
    ) -> Self {
        let mut tasks = Vec::<TranscodeTask<'file>>::default();
        for stream in streams.iter() {
            if *requirement == *stream {
                if let Some(task) = TranscodeTask::<'file>::new(stream, config) {
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

impl<'a> From<Stream<'a>> for StreamCodec<'a> {
    fn from(stream: Stream<'a>) -> Self {
        let codec_parameters = stream.parameters();
        let codec_id = codec_parameters.id();

        let codec = find_codec(codec_id);
        Self { stream, codec }
    }
}

impl<'file> TranscodeTask<'file> {
    pub fn new(stream: &'file StreamCodec<'file>, config: &TranscoderConfig) -> Option<Self> {
        let mut action = None;
        for supp in config.supported_codecs.iter() {
            if let Some(codec) = stream.codec {
                if codec.id() == *supp {
                    action = Some(TranscodeTaskType::Supported);
                    break;
                } else if action.is_none() {
                    let supp = find_codec(*supp);
                    if let Some(supp) = supp {
                        if supp.medium() == codec.medium() {
                            action = Some(TranscodeTaskType::Transcode(supp.id()))
                        }
                    }
                }
            }
        }
        action.map(|action| Self { action, stream })
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

fn deserialize_codecs<'de, D>(deserializer: D) -> Result<Vec<codec::Id>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let ids = Vec::<String>::deserialize(deserializer)?;
    let mut res = Vec::<codec::Id>::new();
    res.reserve(ids.len());
    for id_str in ids.into_iter() {
        let id_str = id_str.to_lowercase();
        let codec = find_codec_by_name(&id_str)
            .ok_or_else(|| serde::de::Error::custom(&format!("Unknown codec {id_str}")))?;
        res.push(codec.id());
    }
    Ok(res)
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
