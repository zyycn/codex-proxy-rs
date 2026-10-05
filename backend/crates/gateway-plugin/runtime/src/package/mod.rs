//! 插件包校验、检查、兼容性与解压资源的公共入口

mod cache;
mod compatibility;
mod icon;
mod inspection;
mod validation;

pub use cache::PreparedPackage;
pub(crate) use compatibility::{
    requirements as compatibility_requirements, warning as compatibility_warning,
};
pub use inspection::PackageInspector;
pub use validation::{PackageError, PackageLimits, ValidatedPackage};
