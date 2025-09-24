use ffmpeg_next as ffmpeg;
use std::io;
use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};

pub struct Transcoder<'a> {
    config: MutexGuard<'a, TranscoderConfig>,
}

enum MediaFile<'a> {
    Input {
        input: ffmpeg::format::context::Input,
        path: &'a Path,
    },
    Other {
        path: &'a Path,
    },
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
    pub fn path(&self) -> &Path {
        return match self {
            Self::Input { input: _, path } => path,
            Self::Other { path } => path,
        };
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
        if self.config.need_to_transcode(&src) {
            todo!();
        } else {
            std::fs::create_dir_all(dst.parent().unwrap_or(Path::new("/")))?;
            std::os::unix::fs::symlink(src.path(), dst)?;
        }
        Ok(())
    }
}

pub struct TranscoderConfig {}

static CONFIG: LazyLock<Mutex<TranscoderConfig>> =
    LazyLock::new(|| Mutex::new(TranscoderConfig::new()));

impl TranscoderConfig {
    pub fn get<'a>() -> MutexGuard<'a, TranscoderConfig> {
        CONFIG.lock().unwrap()
    }

    pub fn new() -> TranscoderConfig {
        TranscoderConfig {}
    }

    pub fn need_to_transcode(&self, src: &MediaFile) -> bool {
        match src {
            MediaFile::Input { input, path: _ } => {
                for (stream_index, stream) in input.streams().enumerate() {
                    let codec_parameters = stream.parameters();
                    let codec_id = codec_parameters.id();

                    println!("Stream {}:", stream_index);

                    for (name, value) in stream.metadata().iter() {
                        println!("  {name}: {value}");
                    }

                    println!("  Codec ID: {:?}", codec_id);

                    if let Some(codec) = ffmpeg::codec::decoder::find(codec_id) {
                        println!("  Codec Name: {}", codec.name());
                        println!("  Codec Long Name: {}", codec.description());
                        println!("  Codec Type: {:?}", codec.medium());
                    } else {
                        println!("  Codec not found for ID: {:?}", codec_id);
                    }
                }
                false
            }
            MediaFile::Other { path: _ } => false,
        }
    }
}
