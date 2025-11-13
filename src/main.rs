use clap::Parser;
use log::{debug, info};
use std::path::PathBuf;
use transcoder;

#[derive(Parser, Debug)]
struct Args {
    input: PathBuf,
    output: PathBuf
}

fn main() {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));
    let args = Args::parse();

    todo!()

}
