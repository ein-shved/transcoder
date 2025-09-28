use clap::Parser;
use log::info;
use tokio;
use transcoder::watcher::{WatchPair, Watcher};

#[derive(Parser, Debug)]
struct Args {
    pair: WatchPair,
    pairs: Vec<WatchPair>,
}

#[tokio::main]
async fn main() {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));
    let args = Args::parse();

    let mut watcher = Watcher::new();
    for pair in std::iter::once(args.pair).chain(args.pairs.into_iter()) {
        info!("Watching {:?} -> {:?}", pair.src, pair.dst);
        watcher.add(pair).unwrap();
    }
    watcher.watch().await;
}
