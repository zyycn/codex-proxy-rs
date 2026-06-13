use std::{fs, io::Write};

use tracing_subscriber::fmt::MakeWriter;

use codex_proxy_rs::codex::logs::rotation::{RotatingLogWriter, RotationConfig};

#[test]
fn rotating_writer_splits_files_when_size_limit_is_reached() {
    let dir = tempfile::tempdir().unwrap();
    let writer = RotatingLogWriter::new(RotationConfig::new(dir.path(), 16, 14)).unwrap();

    {
        let mut file = writer.make_writer();
        file.write_all(b"first log line\n").unwrap();
        file.write_all(b"second log line\n").unwrap();
        file.flush().unwrap();
    }

    let log_files = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("codex-proxy-rs") && name.ends_with(".log"))
        })
        .count();

    assert!(
        log_files >= 2,
        "expected rotated log files, found {log_files}"
    );
}
