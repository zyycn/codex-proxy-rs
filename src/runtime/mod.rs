//! 运行时层：启动编排、共享状态、服务接线和后台任务生命周期。

pub mod bootstrap;
pub mod services;
pub mod shutdown;
pub mod state;
pub mod tasks;
