//! 文件和目录交换共用的恢复顺序，保留主失败及补偿失败

use std::{io, path::Path};

pub(super) fn swap_paths(
    current: &Path,
    backup: &Path,
    swap: &Path,
    mut move_path: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    move_path(current, swap)?;
    if let Err(error) = move_path(backup, current) {
        return Err(with_recovery(
            error,
            "restore current from swap",
            move_path(swap, current),
            [current, backup, swap],
        ));
    }
    if let Err(error) = move_path(swap, backup) {
        // 先把备份版本放回 backup，才能把原版本移回 current
        // 补偿第一步失败时保留 swap，不能覆盖仍在 current 的备份版本
        if let Err(recovery) = move_path(current, backup) {
            return Err(with_recovery(
                error,
                "restore backup from current",
                Err(recovery),
                [current, backup, swap],
            ));
        }
        return Err(with_recovery(
            error,
            "restore current from swap",
            move_path(swap, current),
            [current, backup, swap],
        ));
    }
    Ok(())
}

fn with_recovery(
    primary: io::Error,
    action: &'static str,
    recovery: io::Result<()>,
    [current, backup, swap]: [&Path; 3],
) -> io::Error {
    match recovery {
        Ok(()) => primary,
        Err(recovery) => io::Error::new(
            primary.kind(),
            format!(
                "{primary}; rollback failed: {action}: {recovery}; manual recovery required (current={}, backup={}, swap={})",
                current.display(),
                backup.display(),
                swap.display()
            ),
        ),
    }
}
