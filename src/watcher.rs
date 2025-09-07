use async_inotify::{WatchMask, Watcher as IWatcher};
use inotify::WatchDescriptor;
use std::{collections::HashMap, path::PathBuf, str::FromStr};

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

    pub fn add(&mut self, wp: WatchPair) -> async_inotify::Result<()>
    {
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
                if let Ok(suffix) = event.path().strip_prefix(&wp.src)
                {
                    println!("{:?}: {:?} -> {:?}", event.mask(), event.path(), wp.dst.join(suffix));
                }
                else
                {
                    println!("{:?}: {:?} -> unexpected watching path", event.mask(), event.path());
                }
            } else {
                break;
            }
        }
    }
}
