//! 验证日志压缩、轮转与重启后按日期保留完整记录

use super::*;
use std::time::SystemTime;

const ROTATION_RECORDS: usize = 3;

#[test]
fn retention_preserves_complete_dates_across_compression_rotation_and_restart() {
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        with_file_logging(
            PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap()),
            true,
            || {
                let payload = "x".repeat(1024 * 1024);
                // 三条记录已跨过 1 MiB 轮转边界；保留日期的 25 个分段由下方单独构造
                for sequence in 0..ROTATION_RECORDS {
                    tracing::info!(target: REQUEST_DUMP_LOG_TARGET, sequence, payload, "retention record");
                    tracing::info!(target: APPLICATION_LOG_TARGET, sequence, payload, "retention record");
                    tracing::info!(target: OAUTH_RECOVERY_LOG_TARGET, sequence, payload, "retention record");
                }
            },
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let today = gateway_core::time::DeploymentTimeZone::default()
        .local(chrono::Utc::now())
        .date_naive();
    let mut retained = Vec::new();
    let mut expired = Vec::new();
    for (prefix, days) in [
        (APPLICATION_LOG_FILE_PREFIX, 7),
        (OAUTH_RECOVERY_LOG_FILE_PREFIX, 7),
        (REQUEST_DUMP_LOG_FILE_PREFIX, 1),
    ] {
        let boundary = today - chrono::Days::new(days);
        for segment in 1..=25 {
            let path = directory
                .path()
                .join(format!("{prefix}{boundary}.{segment}.log"));
            seed_log(&path, boundary, &format!("boundary-{segment}\n"));
            retained.push(path);
        }
        let old_date = boundary - chrono::Days::new(1);
        let path = directory.path().join(format!("{prefix}{old_date}.log"));
        seed_log(&path, old_date, "expired date\n");
        expired.push(path);
        // 时区切换或恢复的文件按较近写入日期保护整个分段组
        let restored_date = old_date - chrono::Days::new(1);
        for segment in 0..=1 {
            let path = directory
                .path()
                .join(format!("{prefix}{restored_date}.{segment}.log"));
            seed_log(
                &path,
                if segment == 0 { today } else { restored_date },
                "restored date\n",
            );
            retained.push(path);
        }
    }
    let unrelated = directory.path().join("unmanaged.log");
    fs::write(&unrelated, "unmanaged\n").unwrap();
    // 归档发布前崩溃会留下原文件，也可能留下未完成的临时文件
    fs::write(
        retained[0].with_extension("log.gz.tmp"),
        "unfinished archive",
    )
    .unwrap();
    for run in 1..=2 {
        let output = Command::new(env::current_exe().unwrap())
            .args(["--exact", "logging::writer::retention_preserves_complete_dates_across_compression_rotation_and_restart"])
            .env(CHILD_PROCESS_ENV, "1").env(LOG_DIRECTORY_ENV, directory.path())
            .env("RUST_LOG", "off,logging_test_application=info").output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            retained
                .iter()
                .all(|path| path.with_extension("log.gz").exists())
        );
        assert!(
            expired
                .iter()
                .all(|path| !path.exists() && !path.with_extension("log.gz").exists())
        );
        assert!(unrelated.exists());

        for prefix in [
            APPLICATION_LOG_FILE_PREFIX,
            OAUTH_RECOVERY_LOG_FILE_PREFIX,
            REQUEST_DUMP_LOG_FILE_PREFIX,
        ] {
            let body = read_log_file_set(directory.path(), prefix);
            let mut sequences = [0; ROTATION_RECORDS];
            let mut boundaries = std::collections::BTreeSet::new();
            for line in body.lines() {
                if let Some(segment) = line.strip_prefix("boundary-") {
                    boundaries.insert(segment.parse::<usize>().unwrap());
                } else if line.starts_with('{') {
                    #[derive(serde::Deserialize)]
                    struct Record {
                        fields: Fields,
                    }
                    #[derive(serde::Deserialize)]
                    struct Fields {
                        sequence: Option<usize>,
                    }
                    if let Some(sequence) = serde_json::from_str::<Record>(line)
                        .unwrap()
                        .fields
                        .sequence
                    {
                        sequences[sequence] += 1;
                    }
                }
            }
            assert_eq!(sequences, [run; ROTATION_RECORDS]);
            assert_eq!(
                boundaries,
                (1..=25).collect(),
                "whole boundary date must survive"
            );
            let today_segments = fs::read_dir(directory.path())
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&format!("{prefix}{today}"))
                })
                .count();
            assert!(today_segments >= 2, "records must exercise size rotation");
        }
    }
}

fn seed_log(path: &Path, date: chrono::NaiveDate, body: &str) {
    fs::write(path, body).unwrap();
    let modified = gateway_core::time::DeploymentTimeZone::default()
        .date_start(date)
        .unwrap();
    fs::File::open(path)
        .unwrap()
        .set_modified(SystemTime::from(modified))
        .unwrap();
}
