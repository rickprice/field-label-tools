use clap::Parser;

#[derive(Parser)]
#[command(
    version,
    about = "Convert split-and-tagged photo shoot directories into XMP sidecars for Darktable import (not yet implemented)"
)]
struct Args {}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let _args = Args::parse();
    log::error!("darktable-export: not yet implemented");
    std::process::exit(1);
}
