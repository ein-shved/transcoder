use ez_ffmpeg::codec::{self as ffcodec, CodecInfo};
use log::{debug, trace};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::ops::Deref;
use std::path::Path;
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
    decoder: bool,
}

static CODECS: LazyLock<RwLock<IndexedCodecs>> =
    LazyLock::new(|| RwLock::new(IndexedCodecs::new()));

enum MediaFile<'a> {
    Input {
        // input: context::Input,
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
    Transcode(CodecInfoExtra),
}

impl<'a> MediaFile<'a> {
    pub fn new(path: &'a Path) -> Self {
        todo!();
    }
}

impl fmt::Debug for MediaFile<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!();
    }
}

impl<'a> Transcoder<'a> {
    pub fn get() -> Self {
        Self {
            config: TranscoderConfig::get(),
        }
    }
    pub fn transcode(self, src: &Path, dst: &Path) -> io::Result<()> {
        todo!();
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
    // pub fn new(streams: &Streams, config: &'req TranscoderConfig) -> Self {
    //     let mut tasks = vec![];
    //
    //     for req in config.required.iter() {
    //         tasks.push(RequirementTaks::<'req>::new(config, streams, req));
    //     }
    //     Self { config, tasks }
    // }
    //
    // pub fn need_to_transcode(&self, src: &Path) -> bool {
    //     if let Some(format) = get_format(src) {
    //         let mut format_supported = false;
    //         for supp in self.config.supported_formats.iter() {
    //             if *supp == format {
    //                 format_supported = true;
    //             }
    //         }
    //         if !format_supported {
    //             return true;
    //         }
    //     }
    //     for task in self.tasks.iter() {
    //         if task.need_to_transcode() {
    //             return true;
    //         }
    //     }
    //     false
    // }
    // fn make_program<'a>(&self, src: Streams<'a>, dst: &mut context::Output) -> Vec<ProgramTask<'a>> {
    //     src.into_iter().filter_map(|stream| {
    //         self.make_task_for(stream, dst)
    //     }).collect()
    // }
    // fn make_task_for<'a>(&self, stream: StreamCodec<'a>, dst: &mut context::Output) -> Option<ProgramTask<'a>> {
    //     let task = self.find_task_for(&stream);
    //     let codec = if let Some(task) = task {
    //         match task.action {
    //             TranscodeTaskType::Transcode(ref codec) => Some(codec),
    //             TranscodeTaskType::Supported => stream.codec.as_ref(),
    //         }
    //     } else {
    //         stream.codec.as_ref()
    //     }?;
    //     let mut out_stream = dst.add_stream(*codec).ok()?;
    //     out_stream.set_parameters(stream.stream.parameters());
    //     let mut encoder = codec.encoder()?.video().ok()?;
    //
    //     todo!()
    // }
    // fn find_task_for<'a>(&'a self, stream: &StreamCodec) -> Option<&'a TranscodeTask> {
    //         let mut final_task = None;
    //         for task in self.tasks.iter() {
    //             for task in task.tasks.iter() {
    //                 if task.stream_index == stream.stream.index() {
    //                     final_task = Some(task);
    //                     break;
    //                 }
    //             }
    //             if final_task.is_some() {
    //                 break;
    //             }
    //         }
    //         final_task
    // }
    // pub fn transcode(&self, src: Streams, dst: context::Output) -> io::Result<()> {
    //     dst.write_header();
    //     for StreamCodec {
    //         stream,
    //         packet,
    //         codec,
    //     } in src.into_iter()
    //     {
    //         let mut final_task = None;
    //         for task in self.tasks.iter() {
    //             for task in task.tasks.iter() {
    //                 if task.stream_index == stream.index() {
    //                     final_task = Some(task.clone());
    //                     break;
    //                 }
    //             }
    //             if final_task.is_some() {
    //                 break;
    //             }
    //         }
    //         if let Some(task) = final_task {
    //             task.transcode(stream, packet, codec);
    //         } else {
    //             let dst = dst.add_stream(codec);
    //         }
    //     }
    //     Ok(())
    // }
}

impl<'req> RequirementTaks<'req> {
    // pub fn new(
    //     config: &'req TranscoderConfig,
    //     streams: &Streams,
    //     requirement: &'req Requirement,
    // ) -> Self {
    //     let mut tasks = Vec::<TranscodeTask>::default();
    //     for stream in streams.iter() {
    //         if *requirement == *stream {
    //             if let Some(task) = TranscodeTask::new(stream, config) {
    //                 tasks.push(task);
    //             }
    //         }
    //     }
    //     Self { requirement, tasks }
    // }
    // pub fn get_level(&self) -> RequirementLevel {
    //     self.requirement.level
    // }
    // pub fn need_to_transcode(&self) -> bool {
    //     let level = self.get_level();
    //     if self.tasks.is_empty()
    //         || level == RequirementLevel::WithOther
    //         || level == RequirementLevel::Ignore
    //         || level == RequirementLevel::Decline
    //     {
    //         return false;
    //     }
    //     for task in self.tasks.iter() {
    //         if task.need_to_transcode() {
    //             if level == RequirementLevel::All {
    //                 return true;
    //             }
    //         } else {
    //             if level == RequirementLevel::AtLeastOne {
    //                 return false;
    //             }
    //         }
    //     }
    //     return level == RequirementLevel::AtLeastOne;
    // }
}

// impl PartialEq<StreamCodec<'_>> for Requirement {
//     fn eq(&self, stream: &StreamCodec<'_>) -> bool {
//         self.what == *stream
//     }
// }
//
// impl PartialEq<StreamCodec<'_>> for RequirementType {
//     fn eq(&self, stream: &StreamCodec<'_>) -> bool {
//         use media::Type;
//         if let Some(ref codec) = stream.codec {
//             let media = codec.medium();
//             let media_meta = stream.stream.metadata();
//             let media_lang = media_meta.get("language");
//             match self {
//                 Self::Video => media == Type::Video,
//                 Self::Audio(audio) => {
//                     media == Type::Audio
//                         && (audio.language.is_none() || media_lang == audio.language.as_deref())
//                 }
//                 Self::Subtitle(subs) => {
//                     media == Type::Subtitle
//                         && (subs.language.is_none() || media_lang == subs.language.as_deref())
//                 }
//             }
//         } else {
//             false
//         }
//     }
// }

// impl<'a> From<(Stream<'a>, Packet)> for StreamCodec<'a> {
//     fn from(pair: (Stream<'a>, Packet)) -> Self {
//         let (stream, packet) = pair;
//         let codec_parameters = stream.parameters();
//         let codec_id = codec_parameters.id();
//
//         let codec = find_codec(codec_id);
//         Self {
//             stream,
//             packet,
//             codec,
//         }
//     }
// }

impl<'file> TranscodeTask {
    // pub fn new(stream: &'file StreamCodec<'file>, config: &TranscoderConfig) -> Option<Self> {
    //     let mut action = None;
    //     for supp in config.supported_codecs.iter() {
    //         if let Some(codec) = stream.codec {
    //             if codec == *supp {
    //                 action = Some(TranscodeTaskType::Supported);
    //                 break;
    //             } else if action.is_none() {
    //                 if supp.medium() == codec.medium() {
    //                     action = Some(TranscodeTaskType::Transcode(*supp))
    //                 }
    //             }
    //         }
    //     }
    //     action.map(|action| Self {
    //         action,
    //         stream_index: stream.stream.index(),
    //     })
    // }
    // pub fn need_to_transcode(&self) -> bool {
    //     return self.action != TranscodeTaskType::Supported;
    // }
}

impl fmt::Debug for TranscodeTaskType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Supported => write!(f, "Supported"),
            Self::Transcode(codec) => write!(f, "Transcode to {}", codec.codec_long_name),
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
