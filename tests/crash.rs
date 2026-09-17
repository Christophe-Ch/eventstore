use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

use eventstore::{error::Result, log::Log};

#[test]
fn acknowledged_records_survive_process_kill() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let file_path = temp_dir.path().join("test");
    let appender_path = env!("CARGO_BIN_EXE_appender");
    let mut child = Command::new(appender_path)
        .arg(&file_path)
        .stdout(Stdio::piped())
        .spawn()?;

    let mut reader = BufReader::new(child.stdout.take().expect("expected stdout"));
    let mut offsets = Vec::<u64>::new();

    for _ in 0..20 {
        let mut buffer = String::new();
        let read_bytes = reader.read_line(&mut buffer)?;
        assert!(read_bytes > 0, "child exited after {} lines", offsets.len());

        offsets.push(buffer.trim().parse::<u64>().unwrap());
    }

    child.kill()?;
    child.wait()?;

    let log = Log::open(&file_path)?;
    for expected_offset in offsets {
        let payload = log.read_at(expected_offset)?;
        assert_eq!(payload, b"payload");
    }

    Ok(())
}
