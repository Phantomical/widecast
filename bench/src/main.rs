use clap::Parser;
use widecast_bench::*;

fn main() {
    // console_subscriber::init();

    let config = Config::parse();
    let state = config.test_loaded_latency();

    println!("{state}");
}
