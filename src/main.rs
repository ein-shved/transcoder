use clap::Parser;
use log::error;
use std::path::PathBuf;
use transcoder;

#[derive(Parser, Debug)]
struct Args {
    input: PathBuf,
    output: PathBuf,
}

fn main() -> Result<(), ffmpeg_next::Error> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));
    let args = Args::parse();

    let res = transcoder::transcode(&args.input, &args.output);

    if let Err(err) = res {
        error!("Failed to transcode: {err:#?}");
    }

    res
}
