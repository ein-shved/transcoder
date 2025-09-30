use clap::Parser;
use log::{debug, info};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::process::Stdio;
use tokio;
use transcoder::transcoder::TranscoderConfig;
use transcoder::watcher::{WatchPair, Watcher};

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, required = true)]
    config: PathBuf,
    pair: WatchPair,
    pairs: Vec<WatchPair>,
}

#[tokio::main]
async fn main() {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));
    let args = Args::parse();

    let config_type = args
        .config
        .extension()
        .expect("Unknown config file type")
        .to_str()
        .expect("Unknown config file type")
        .to_lowercase();
    let mut reader = File::open(&args.config).unwrap();

    let config: TranscoderConfig = if config_type == "toml" {
        let mut s = String::new();
        reader.read_to_string(&mut s).unwrap();
        toml::from_str(&s).unwrap()
    } else if config_type == "json" {
        serde_json::from_reader(reader).unwrap()
    } else if config_type == "yaml" {
        serde_yaml::from_reader(reader).unwrap()
    } else if config_type == "nix" {
        let stdout = std::process::Command::new("nix-instantiate")
            .args(["--eval", "--json", "--strict"])
            .arg(&args.config)
            .stderr(Stdio::inherit())
            .output()
            .expect("Unable to process nix config").stdout;
        let json = String::from_utf8(stdout).unwrap();
        serde_json::from_str(&json).unwrap()
    } else {
        panic!("Unsupported config type ${config_type}")
    };
    debug!("Configuration: {config:#?}");
    TranscoderConfig::set(config);

    let mut watcher = Watcher::new();
    for pair in std::iter::once(args.pair).chain(args.pairs.into_iter()) {
        info!("Watching {:?} -> {:?}", pair.src, pair.dst);
        watcher.add(pair).unwrap();
    }
    watcher.watch().await;
}
