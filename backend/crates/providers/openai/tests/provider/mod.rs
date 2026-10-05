//! OpenAI Provider 执行合同与失败处理的测试入口

mod contract;
mod failure;
mod live;

pub(crate) use contract::assert_local_connection_capacity_is_not_an_upstream_failure;
