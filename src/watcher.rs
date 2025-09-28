use async_inotify::{WatchMask, Watcher as IWatcher};
use inotify::{EventMask, WatchDescriptor};
use log::{debug, trace, warn};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    str::FromStr,
};
use tokio::fs::{metadata, read_dir, remove_dir_all, remove_file, symlink_metadata};

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
        let wd = self.watcher.add(
            &wp.src,
            &WatchMask::CREATE
                .union(WatchMask::DELETE)
                .union(WatchMask::MOVED_TO)
                .union(WatchMask::MOVED_FROM)
                .union(WatchMask::CLOSE_WRITE),
        )?;
        Self::recheck(&wp.src, &wp.dst);
        self.descriptors.insert(wd, wp);
        Ok(())
    }

    pub async fn watch(&mut self) {
        loop {
            if let Some(event) = self.watcher.next().await {
                let wp = &self.descriptors[event.wd()];
                let src = event.path().to_owned();
                let mask = event.mask().clone();
                let wp_src = wp.src.clone();
                let wp_dst = wp.dst.clone();
                tokio::spawn(async move {
                    Self::do_action(&mask, &src, &wp_src, &wp_dst, false).await;
                });
            } else {
                break;
            }
        }
    }

    async fn do_action(event: &EventMask, f: &Path, src: &Path, dst: &Path, check_exists: bool) {
        if let Ok(suffix) = f.strip_prefix(src) {
            let dst = dst.join(suffix);
            if dst == f {
                warn!("Source and destination are same: {f:?}");
                return;
            }
            trace!("Processing {event:?} on {f:?}");
            if event.intersects(EventMask::DELETE.union(EventMask::MOVED_FROM)) {
                debug!("Removing {dst:?}");
                if let Err(err) = Self::delete(&dst).await {
                    warn!("Failed to delete {dst:?}: {err:?}");
                }
            } else if event.intersects(
                EventMask::CREATE
                    .union(EventMask::MOVED_TO)
                    .union(EventMask::CLOSE_WRITE),
            ) {
                if Self::is_dir(f).await {
                    trace!("Ignoring directory {f:?}")
                } else {
                    if dst.exists() && check_exists {
                        trace!("Ignoring existed {f:?}")
                    } else {
                        debug!("Performing emplacing {f:?} to {dst:?}");
                        if let Err(err) = Transcoder::get().transcode(f, &dst) {
                            warn!("Failed to transcode {src:?} into {dst:?}: {err}");
                        }
                    }
                }
            } else {
                warn!("{:?}: {:?} -> unexpected event", event, f);
            }
        } else {
            warn!("{:?}: {:?} -> unexpected watching path", event, f);
        }
    }

    fn recheck(src: &Path, dst: &Path) {
        let src = src.to_owned();
        let dst = dst.to_owned();
        tokio::spawn(async move { Self::check_f(&src, &src, &dst).await });
    }

    async fn check_f(f: &Path, src: &Path, dst: &Path) {
        trace!("Rechecking {f:?} ({src:?} -> {dst:?})");
        if Self::is_dir(f).await {
            if let Ok(mut dir) = read_dir(f).await {
                while let Ok(f) = dir.next_entry().await {
                    if let Some(f) = f {
                        Box::pin(Self::check_f(&f.path(), src, dst)).await
                    } else {
                        break;
                    }
                }
            }
        } else {
            Self::do_action(&EventMask::CREATE, f, src, dst, true).await;
        }
    }

    async fn delete(p: &Path) -> io::Result<()> {
        let stat = symlink_metadata(p).await?;
        if stat.file_type().is_dir() {
            remove_dir_all(p).await?;
        } else {
            remove_file(p).await?;
        };
        Ok(())
    }

    async fn is_dir(p: &Path) -> bool {
        if let Ok(stat) = metadata(p).await {
            stat.is_dir()
        } else {
            true // Assume is_dir in case of error to make process ignores path.
        }
    }
}
