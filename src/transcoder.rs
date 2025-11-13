use ffmpeg_next::codec::Context;
use ffmpeg_next::format::context;
use ffmpeg_next::{self as ffmpeg, Codec, Packet, Stream, StreamMut, codec, encoder, media};
use log::{debug, trace};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::{fmt, fs, io};

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
    #[serde(default)]
    backup_symlink: bool,
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

trait TranscoderImpl<Output> {
    type ErrType;
    fn transcode(self, output: Output, config: &TranscoderConfig) -> Result<(), Self::ErrType>;
}

#[derive(Debug)]
struct MediaFileTasks<'req> {
    config: &'req TranscoderConfig,
    tasks: Vec<RequirementTaks<'req>>,
}

struct MediaOutputTasks {
    dst: context::Output,
    tasks: HashMap<usize, StreamOutputTask>,
}

#[derive(Debug)]
struct RequirementTaks<'req> {
    requirement: &'req Requirement,
    tasks: Vec<TranscodeTask>,
}

struct TranscodePair {
    decoder: Box<dyn DerefMut<Target = ffmpeg::decoder::Opened>>,
    encoder: Box<dyn DerefToEncoder>,
    resempler: Option<ffmpeg::software::resampling::Context>,
    output_index: usize,
}

struct StreamOutputTask {
    transcoder: Option<TranscodePair>,
    output_index: usize,
    input_index: usize,
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

struct StreamCodec<'a> {
    stream: Stream<'a>,
    codec: Option<Codec>,
}

impl<'a> MediaFile<'a> {
    pub fn new(path: &'a Path) -> Self {
        let input = ffmpeg_next::format::input(path);
        if let Ok(input) = input {
            Self::Input { input, path }
        } else {
            Self::Other { path }
        }
    }
}

impl<'a> TranscoderImpl<&Path> for MediaFile<'a> {
    type ErrType = io::Error;
    fn transcode(self, output: &Path, config: &TranscoderConfig) -> io::Result<()> {
        match self {
            Self::Input { input, path: src } => input.transcode((src, output), config),
            Self::Other { path } => path.transcode(output, config),
        }
    }
}

impl TranscoderImpl<(&Path, &Path)> for context::Input {
    type ErrType = io::Error;

    fn transcode(
        mut self,
        (src, dst): (&Path, &Path),
        config: &TranscoderConfig,
    ) -> Result<(), Self::ErrType> {
        let tasks = MediaFileTasks::new(&self, config);

        trace!("Tasks: {tasks:#?}");
        if tasks.need_to_transcode(src) {
            prepare_dirs(dst)?;
            let dst_p = if let Some(ext) = config.supported_formats.get(0) {
                dst.with_extension(ext)
            } else {
                dst.to_owned()
            };
            let dst_ctx = ffmpeg::format::output(&dst_p)?;
            let mut tasks = MediaOutputTasks::new(tasks, &self, dst_ctx)?;
            tasks.dst.set_metadata(self.metadata().to_owned());
            let res = tasks.transcode(&mut self);
            if res.is_ok() {
                debug!("Transcoding {:?} -> {:?} successful", src, dst);
                res
            } else {
                _ = fs::remove_file(dst_p);
                if config.backup_symlink {
                    debug!(
                        "Transcoding {:?} -> {:?} failed! Backuping with symlink",
                        src, dst
                    );
                    _ = src.transcode(dst, config);
                }
                res
            }
        } else {
            src.transcode(dst, config)
        }
    }
}

impl<'a> TranscoderImpl<&Path> for &'a Path {
    type ErrType = io::Error;

    fn transcode(self, dst: &Path, _config: &TranscoderConfig) -> io::Result<()> {
        prepare_dirs(dst)?;
        trace!("Symlink {self:#?} -> {dst:#?}");
        std::os::unix::fs::symlink(self, dst)
    }
}

fn prepare_dirs(dst: &Path) -> io::Result<()> {
    let parent = dst.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid destination without parent",
        )
    })?;

    std::fs::create_dir_all(parent)
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
        let file = MediaFile::new(src);
        debug!("{file:#?}");
        file.transcode(dst, &self.config)
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
    pub fn new(input: &context::Input, config: &'req TranscoderConfig) -> Self {
        let mut tasks = vec![];

        for req in config.required.iter() {
            tasks.push(RequirementTaks::<'req>::new(config, input, req));
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
}

impl MediaOutputTasks {
    pub fn new(
        tasks: MediaFileTasks<'_>,
        src: &context::Input,
        mut dst: context::Output,
    ) -> io::Result<Self> {
        let mut output_tasks = HashMap::default();
        for stream in src.streams() {
            let mut final_task = None;
            for task in tasks.tasks.iter() {
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
            let codec = find_codec(stream.parameters().id());
            output_tasks.insert(
                stream.index(),
                StreamOutputTask::new(stream, codec, &mut dst, final_task)?,
            );
        }
        Ok(Self {
            dst,
            tasks: output_tasks,
        })
    }
    pub fn transcode(&mut self, src: &mut context::Input) -> io::Result<()> {
        self.dst.write_header()?;

        for (stream, packet) in src.packets() {
            let task = self.tasks.get_mut(&stream.index());
            if let Some(task) = task {
                task.process_packet(packet, &mut self.dst)?;
            }
        }
        for (_, task) in self.tasks.iter_mut() {
            task.send_eof(&mut self.dst)?;
        }

        self.dst.write_trailer()?;
        Ok(())
    }
}

impl StreamOutputTask {
    pub fn new(
        stream: Stream,
        decodec: Option<Codec>,
        dst: &mut context::Output,
        task: Option<TranscodeTask>,
    ) -> io::Result<Self> {
        let encodec = task.and_then(|task| match task.action {
            TranscodeTaskType::Supported => None,
            TranscodeTaskType::Transcode(codec) => Some(codec),
        });
        let need_to_transcode = encodec.is_some() && decodec.is_some();
        let encodec = encodec.or(decodec);
        // TODO: Remove
        // let encodec = decodec;
        // need_to_transcode = false;

        let encodec = encodec.map(|e| ffmpeg::encoder::find(e.id()).unwrap() );
        trace!(
            "New output task: {:?} -> {:?}",
            decodec.map(|e| e.name().to_owned()),
            encodec.map(|e| e.name().to_owned())
        );
         let mut output_stream = dst.add_stream(encodec.clone())?;
        //let mut output_stream = dst.add_stream(decodec.clone())?;
        debug!("Encodec1: {:?}", output_stream.parameters().id());
        let output_index = output_stream.index();

        if !need_to_transcode {
            output_stream.set_parameters(stream.parameters());
        }
        debug!("Encodec2: {:?}", output_stream.parameters().id());
        output_stream.set_metadata(stream.metadata().to_owned());
        debug!("Encodec3: {:?}", output_stream.parameters().id());
        let transcoder = if need_to_transcode {
            Some(TranscodePair::new(
                &stream,
                &mut output_stream,
                decodec.unwrap(),
                encodec.unwrap(),
            )?)
        } else {
            None
        };

        Ok(Self {
            transcoder,
            output_index,
            input_index: stream.index(),
        })
    }

    pub fn process_packet(
        &mut self,
        mut packet: Packet,
        dst: &mut context::Output,
    ) -> io::Result<()> {
        if let Some(ref mut transcoder) = self.transcoder {
            debug!("Transcode 1");
            transcoder.process_packet(packet, dst)?;
            debug!("Transcode 2");
        } else {
            packet.set_stream(self.output_index);
            packet.write(dst)?;
        }
        Ok(())
    }
    pub fn send_eof(&mut self, dst: &mut context::Output) -> io::Result<()> {
        if let Some(ref mut transcoder) = self.transcoder {
            transcoder.send_eof(dst)?;
        }
        Ok(())
    }
}

impl TranscodePair {
    pub fn new(
        src: &Stream<'_>,
        dst: &mut StreamMut,
        decodec: Codec,
        encodec: Codec,
    ) -> io::Result<Self> {
        let encodec = decodec;
        let decoder = Context::from_parameters(src.parameters())?.decoder();
        debug!("Encodec: {:?}", dst.parameters().id());
        let encodec = find_codec(dst.parameters().id()).unwrap();

        let mut encoder = Context::new_with_codec(encodec).encoder();

        encoder.set_frame_rate(Some(decoder.frame_rate()));
        dst.set_parameters(&encoder);

        let res = match decodec.medium() {
            media::Type::Audio => {
                let mut en_audio = encoder.audio()?;
                let mut de_audio = decoder.audio()?;
                let encodec = encodec.audio()?;
                let channel_layout = encodec
                    .audio()?
                    .channel_layouts()
                    .map(|cls| cls.best(de_audio.channel_layout().channels()))
                    .unwrap_or(ffmpeg::channel_layout::ChannelLayout::STEREO);
                de_audio.set_parameters(src.parameters())?;

                en_audio.set_rate(de_audio.rate() as i32);
                en_audio.set_channel_layout(channel_layout);
                en_audio.set_bit_rate(de_audio.bit_rate());
                en_audio.set_max_bit_rate(de_audio.max_bit_rate());
                en_audio.set_format(
                    encodec
                        .formats()
                        .ok_or(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Can not get available formats of audio codec",
                        ))?
                        .next()
                        .ok_or(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Audio codec contains no formats",
                        ))?,
                );

                en_audio.set_channel_layout(de_audio.channel_layout());

                en_audio.set_time_base((1, de_audio.rate() as i32));
                dst.set_time_base((1, de_audio.rate() as i32));
                dst.set_parameters(&en_audio);

                debug!(
                    "Encoder: format={:?}, channels={:?}, rate={:?}, layout={:?} bitrate={} time_base={}",
                    en_audio.format(),
                    en_audio.channels(),
                    en_audio.rate(),
                    en_audio.channel_layout(),
                    unsafe { (*en_audio.as_mut_ptr()).bit_rate },
                    en_audio.time_base()
                );
                debug!("Encodec: {:?}", encodec.name());
                debug!("IsOpen: {}", unsafe {
                    ffmpeg_next::ffi::avcodec_is_open(en_audio.as_mut_ptr())
                });

                let mut en_audio = en_audio.open()?;
                // let mut en_audio = en_audio.open_as(encodec)?;
                debug!("IsOpen: {}", unsafe {
                    ffmpeg_next::ffi::avcodec_is_open(en_audio.as_mut_ptr())
                });
                debug!(
                    "Encoder2: format={:?}, channels={:?}, rate={:?}, layout={:?} bitrate={} time_base={}",
                    en_audio.format(),
                    en_audio.channels(),
                    en_audio.rate(),
                    en_audio.channel_layout(),
                    unsafe { (*en_audio.as_mut_ptr()).bit_rate },
                    en_audio.time_base()
                );
                debug!("Encodec2: {:?}", en_audio.codec().unwrap().name());
                let resempler = ffmpeg::software::resampler(
                    (
                        de_audio.format(),
                        de_audio.channel_layout(),
                        de_audio.rate(),
                    ),
                    (
                        en_audio.format(),
                        en_audio.channel_layout(),
                        en_audio.rate(),
                    ),
                )?;

                Self {
                    decoder: Box::new(de_audio),
                    encoder: Box::new(en_audio),
                    resempler: None,
                    // resempler: Some(resempler),
                    output_index: dst.index(),
                }
            }
            media::Type::Video => {
                let mut en_video = encoder.video()?;
                let mut de_video = decoder.video()?;

                de_video.set_parameters(src.parameters())?;
                en_video.set_parameters(src.parameters())?;
                en_video.set_format(de_video.format());
                en_video.set_height(de_video.height());
                en_video.set_width(de_video.width());
                en_video.set_aspect_ratio(de_video.aspect_ratio());
                en_video.set_colorspace(de_video.color_space());
                en_video.set_color_range(de_video.color_range());

                let en_video = en_video.open()?;
                Self {
                    decoder: Box::new(de_video),
                    encoder: Box::new(en_video),
                    resempler: None,
                    output_index: dst.index(),
                }
            }
            media::Type::Subtitle => {
                let mut en_sub = encoder.subtitle()?;
                let mut de_sub = decoder.subtitle()?;

                de_sub.set_parameters(src.parameters())?;
                en_sub.set_parameters(src.parameters())?;

                let mut en_sub = en_sub.open()?;
                Self {
                    decoder: Box::new(de_sub),
                    encoder: Box::new(en_sub),
                    resempler: None,
                    output_index: dst.index(),
                }
            }
            _ => {
                panic!("Do not know how to transcode stream");
            }
        };
        Ok(res)
    }

    pub fn process_packet(&mut self, packet: Packet, dst: &mut context::Output) -> io::Result<()> {
        debug!("Transcode 2");
        self.decoder.send_packet(&packet)?;
        debug!("Transcode 2.1");
        self.decode(dst)?;
        debug!("Transcode 2.2");
        Ok(())
    }

    pub fn decode(&mut self, dst: &mut context::Output) -> io::Result<()> {
        let mut decoded = ffmpeg::frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let timestamp = decoded.timestamp();
            decoded.set_pts(timestamp);
            debug!("Transcode 3");
            debug!(
                "Decoded: format={:?}, channels={:?}, rate={:?}, layout={:?}, ts={:?}",
                decoded.format(),
                decoded.channels(),
                decoded.rate(),
                decoded.channel_layout(),
                decoded.timestamp(),
            );
                debug!("IsOpen: {}", unsafe {
                    ffmpeg_next::ffi::avcodec_is_open(self.encoder.deref_mut_to_encoder().as_mut_ptr())
                });
            if let Some(resempler) = self.resempler.as_mut() {
                debug!("Transcode 3.1");
                let mut resempled = ffmpeg::frame::Audio::empty();
                resempler.run(&decoded, &mut resempled)?;
            debug!(
                "Resempled: format={:?}, channels={:?}, rate={:?}, layout={:?}, ts={:?}",
                resempled.format(),
                resempled.channels(),
                resempled.rate(),
                resempled.channel_layout(),
                resempled.timestamp(),
            );
                debug!("Transcode 3.2");
                self.encoder.deref_mut_to_encoder().send_frame(&resempled)?;
                debug!("Transcode 3.3");
            } else {
                debug!("Transcode 3.4");
                self.encoder.deref_mut_to_encoder().send_frame(&decoded)?;
                debug!("Transcode 3.5");
            }
            self.encode(dst)?;
            debug!("Transcode 3.6");
        }
        Ok(())
    }
    pub fn encode(&mut self, dst: &mut context::Output) -> io::Result<()> {
        let mut encoded = ffmpeg::Packet::empty();
        while self
            .encoder
            .deref_mut_to_encoder()
            .receive_packet(&mut encoded)
            .is_ok()
        {
            encoded.set_stream(self.output_index);
            encoded.write(dst)?;
        }
        Ok(())
    }
    pub fn send_eof(&mut self, dst: &mut context::Output) -> io::Result<()> {
        self.decoder.send_eof()?;
        self.decode(dst)?;
        Ok(())
    }
}

trait DerefToEncoder {
    fn deref_mut_to_encoder(&mut self) -> &mut ffmpeg::encoder::Encoder;
}

impl DerefToEncoder for ffmpeg::encoder::audio::Encoder {
    fn deref_mut_to_encoder(&mut self) -> &mut ffmpeg_next::encoder::Encoder {
        self
    }
}

impl DerefToEncoder for ffmpeg::encoder::video::Encoder {
    fn deref_mut_to_encoder(&mut self) -> &mut ffmpeg_next::encoder::Encoder {
        self
    }
}

impl DerefToEncoder for ffmpeg::encoder::subtitle::Encoder {
    fn deref_mut_to_encoder(&mut self) -> &mut ffmpeg_next::encoder::Encoder {
        self
    }
}

impl<'req> RequirementTaks<'req> {
    pub fn new(
        config: &'req TranscoderConfig,
        input: &context::Input,
        requirement: &'req Requirement,
    ) -> Self {
        let mut tasks = Vec::<TranscodeTask>::default();
        for stream in input.streams() {
            if *requirement == stream {
                if let Some(task) = TranscodeTask::new(&stream, config) {
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

impl PartialEq<Stream<'_>> for Requirement {
    fn eq(&self, stream: &Stream<'_>) -> bool {
        self.what == *stream
    }
}

impl PartialEq<Stream<'_>> for RequirementType {
    fn eq(&self, stream: &Stream<'_>) -> bool {
        use media::Type;
        let codec = find_codec(stream.parameters().id());
        if let Some(ref codec) = codec {
            let media = codec.medium();
            let media_meta = stream.metadata();
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

impl<'file> TranscodeTask {
    pub fn new(stream: &'file Stream<'file>, config: &TranscoderConfig) -> Option<Self> {
        let mut action = None;
        for supp in config.supported_codecs.iter() {
            let codec = find_codec(stream.parameters().id());
            if let Some(codec) = codec {
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
            stream_index: stream.index(),
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
