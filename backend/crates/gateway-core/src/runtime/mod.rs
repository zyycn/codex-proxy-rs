//! 运行时快照的原子发布、健康状态与跨进程版本收敛

mod publications;
mod publisher;

pub use publications::{
    RuntimeSnapshotHandle, RuntimeSnapshotUnavailable, WeakRuntimeSnapshotHandle,
};
pub use publisher::{
    RuntimeSnapshotPublisher, SnapshotControl, SnapshotRevisionStream, SnapshotSubscriptionError,
    SnapshotSubscriptionPort, runtime_revision_needs_refresh,
};
