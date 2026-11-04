//! 用量明细、聚合维护与纯计价的公开合同

mod value;
pub use value::*;
mod record;
pub use record::*;
mod query;
pub use query::*;
mod pricing;
pub use pricing::*;
mod error;
pub use error::*;

pub const USAGE_DATA_MAX_TARGETS: usize = 500;
pub const USAGE_DATA_MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
pub const USAGE_DATA_MAX_PAGE_SIZE: u16 = 200;

mod operations;
pub use operations::*;

mod aggregate;
pub use aggregate::*;

mod read;
pub use read::*;

pub const RECORDS_SCAN: &str = "host.usage.records.scan";
pub const STATISTICS_QUERY: &str = "host.usage.statistics.query";
pub const READ_NEXT: &str = "host.usage.read.next";
pub const READ_CLOSE: &str = "host.usage.read.close";
