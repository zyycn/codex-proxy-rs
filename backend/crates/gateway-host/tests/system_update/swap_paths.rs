//! 对真实暂存文件注入移动双故障，验证恢复次序和人工恢复线索

#[path = "../../src/system_update/swap_paths.rs"]
mod implementation;

use std::{fs, io};

#[test]
fn failed_swap_preserves_both_versions_and_reports_compensation_failures() {
    for directory in [false, true] {
        for primary in [2, 3] {
            let mut recoveries = vec![None, Some(primary + 1)];
            if primary == 3 {
                recoveries.push(Some(5));
            }
            for recovery in recoveries {
                let temp = tempfile::tempdir().unwrap();
                let current = temp.path().join("current");
                let backup = temp.path().join("backup");
                let swap = temp.path().join("swap");
                for (path, content) in [(&current, "current version"), (&backup, "backup version")]
                {
                    if directory {
                        fs::create_dir(path).unwrap();
                        fs::write(path.join("asset"), content).unwrap();
                    } else {
                        fs::write(path, content).unwrap();
                    }
                }
                let mut moves = 0;
                let error = implementation::swap_paths(&current, &backup, &swap, |from, to| {
                    moves += 1;
                    if moves == primary {
                        return Err(io::Error::other("primary injected failure"));
                    }
                    if recovery == Some(moves) {
                        return Err(io::Error::other("recovery injected failure"));
                    }
                    fs::rename(from, to)
                })
                .unwrap_err();
                let message = error.to_string();
                assert!(message.contains("primary injected failure"));
                if recovery.is_some() {
                    assert!(message.contains("recovery injected failure"));
                    assert!(message.contains("restore "));
                    assert!(message.contains("manual recovery required"));
                    assert!(message.contains(swap.to_str().unwrap()));
                } else {
                    assert!(!message.contains("rollback failed"));
                }
                let mut contents = [&current, &backup, &swap]
                    .into_iter()
                    .filter_map(|path| {
                        fs::read_to_string(if directory {
                            path.join("asset")
                        } else {
                            path.clone()
                        })
                        .ok()
                    })
                    .collect::<Vec<_>>();
                contents.sort();
                assert_eq!(contents, ["backup version", "current version"]);
                if recovery.is_none() {
                    let original = if directory {
                        current.join("asset")
                    } else {
                        current.clone()
                    };
                    assert_eq!(fs::read_to_string(original).unwrap(), "current version");
                }
            }
        }
    }
}
