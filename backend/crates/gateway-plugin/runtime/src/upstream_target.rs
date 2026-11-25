//! 上游适配声明与受管回调共用的不可变请求目标

use std::collections::BTreeMap;

use gateway_admin::model::AdminError;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::upstream_adapter::{UpstreamAdapterDeclaration, UpstreamPathPurpose},
};
use url::Url;

pub(crate) struct UpstreamTarget {
    base: Url,
    paths: BTreeMap<String, UpstreamPathPurpose>,
}

impl UpstreamTarget {
    pub(crate) fn compile(declaration: &UpstreamAdapterDeclaration) -> Result<Self, AdminError> {
        let base = Url::parse(&declaration.base_url)
            .map_err(|_| AdminError::invalid("上游 Base URL 无效"))?;
        let paths = declaration
            .paths
            .iter()
            .map(|path| (path.path.clone(), path.purpose))
            .collect();
        Ok(Self { base, paths })
    }

    pub(crate) fn resolve(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> Result<(Url, UpstreamPathPurpose), PluginFault> {
        let purpose = self
            .paths
            .get(path)
            .copied()
            .unwrap_or(UpstreamPathPurpose::Inference);
        let mut target = self.base.join(path).map_err(|_| invalid())?;
        if !matches!(target.scheme(), "http" | "https") || target.host_str().is_none() {
            return Err(invalid());
        }
        if !query.is_empty() {
            target
                .query_pairs_mut()
                .extend_pairs(query.iter().map(|(key, value)| (key, value)));
        }
        Ok((target, purpose))
    }
}

fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "upstream target is invalid")
}
