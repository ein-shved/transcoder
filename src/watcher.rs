use async_inotify::{WatchMask, Watcher as IWatcher};
use inotify::{EventMask, WatchDescriptor};
use std::{
    collections::HashMap,
    fs::{metadata, remove_dir_all, remove_file, symlink_metadata},
    io,
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::transcoder::Transcoder;

pub struct Watcher {
    watcher: IWatcher,
    descriptors: HashMap<WatchDescriptor, WatchPair>,
}

#[derive(Clone, Debug)]
pub struct WatchPair {
    pub src: PathBuf,
    pub dst: PathBuf,
}

impl FromStr for WatchPair {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut it = s.splitn(2, &[':', ',', '=', ';', ' '][..]);
        let src = it.next().ok_or("Invalid format of watch pair")?.into();
        let dst = it.next().ok_or("Invalid format of watch pair")?.into();
        Ok(Self { src, dst })
    }
}

impl Watcher {
    pub fn new() -> Self {
        Self {
            watcher: IWatcher::init(),
            descriptors: Default::default(),
        }
    }

    pub fn add(&mut self, wp: WatchPair) -> async_inotify::Result<()> {
        let wd = self
            .watcher
            .add(&wp.src, &WatchMask::CREATE.union(WatchMask::DELETE))?;
        self.descriptors.insert(wd, wp);
        Ok(())
    }

    pub async fn watch(&mut self) {
        loop {
            if let Some(event) = self.watcher.next().await {
                let wp = &self.descriptors[event.wd()];
                let src = event.path();
                if let Ok(suffix) = src.strip_prefix(&wp.src) {
                    let dst = wp.dst.join(suffix);
                    if dst == src {
                        continue;
                    }
                    if event.mask().intersects(EventMask::DELETE) {
                        if let Err(err) = Self::delete(&dst){
                            println!("Failed to delete {dst:?}: {err:?}");
                        }
                    } else if event.mask().intersects(EventMask::CREATE) {
                        if !Self::is_dir(src) {
                            _ = Transcoder::get().transcode(src, &dst);
                        }
                    } else {
                        println!("{:?}: {:?} -> unexpected event", event.mask(), src);
                    }
                } else {
                    println!("{:?}: {:?} -> unexpected watching path", event.mask(), src);
                }
            } else {
                break;
            }
        }
    }

    fn delete(p: &Path) -> io::Result<()> {
        let stat = symlink_metadata(p)?;
        if stat.file_type().is_dir() {
            remove_dir_all(p)?;
        } else {
            remove_file(p)?;
        };
        Ok(())
    }

    fn is_dir(p: &Path) -> bool {
        if let Ok(stat) = metadata(p)
        {
            stat.is_dir()
        } else {
            true // Assume is_dir in case of error to make process ignores path.
        }
    }
}
