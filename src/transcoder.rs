use std::io;
use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};


pub struct Transcoder<'a> {
    config: MutexGuard<'a, TranscoderConfig>
}

impl<'a> Transcoder<'a> {
    pub fn get() -> Self
    {
        Self {
            config: TranscoderConfig::get(),
        }
    }
    pub fn transcode(self, src: &Path, dst: &Path) -> io::Result<()>
    {
        if self.config.need_to_transmute(src) {
            todo!()
        } else {
            std::os::unix::fs::symlink(src, dst)?;
        }
        Ok(())
    }
}

pub struct TranscoderConfig {
}

static CONFIG: LazyLock<Mutex<TranscoderConfig>> = LazyLock::new(|| Mutex::new(TranscoderConfig::new()));

impl TranscoderConfig {
    pub fn get<'a>() -> MutexGuard<'a, TranscoderConfig> {
        CONFIG.lock().unwrap()
    }

    pub fn new() -> TranscoderConfig {
        TranscoderConfig {  }
    }

    pub fn need_to_transmute(&self, _src: &Path) -> bool
    {
        return false;
    }
}
