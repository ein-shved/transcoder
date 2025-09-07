use clap::Parser;
use tokio;
use transcoder::watcher::{Watcher, WatchPair};

#[derive(Parser, Debug)]
struct Args {
    pair: WatchPair,
    pairs: Vec<WatchPair>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let mut watcher = Watcher::new();
    for pair in std::iter::once(args.pair).chain(args.pairs.into_iter()) {
        println!("Watching {:?} -> {:?}", pair.src, pair.dst);
        watcher.add(pair).unwrap();
    }
    watcher.watch().await;
}
