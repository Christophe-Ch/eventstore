use std::env;

use eventstore::{error::Result, log::Log};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let file_path = args.get(1).expect("missing file path");

    let mut log = Log::open(file_path)?;
    loop {
        let offset = log.append(b"payload")?;
        println!("{offset}");
    }
}
