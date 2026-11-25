use clap::Parser;

#[derive(Parser)]
#[command(
    version,
    about = "Split a directory of time-ordered photos into per-shoot folders using QR-code marker frames (not yet implemented)"
)]
struct Args {}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let _args = Args::parse();
    log::error!("photo-splitter: not yet implemented");
    std::process::exit(1);
}
