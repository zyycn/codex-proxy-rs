//! 编译插件上游目标与路径约束，解析受管请求目标

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
    pub(super) fn compile(declaration: &UpstreamAdapterDeclaration) -> Result<Self, AdminError> {
        let base = Url::parse(&declaration.base_url)
            .map_err(|_| AdminError::invalid("上游 Base URL 无效"))?;
        if !matches!(base.scheme(), "https" | "http")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || !base.path().ends_with('/')
            || declaration.base_url.len() > 2048
            || declaration.paths.is_empty()
            || declaration.paths.len() > 16
        {
            return Err(AdminError::invalid(
                "上游 Base URL 须为无凭据、查询串或片段且以 / 结尾的 HTTP(S) 地址",
            ));
        }
        let mut paths = BTreeMap::new();
        for path in &declaration.paths {
            if !valid_path(&path.path) || paths.insert(path.path.clone(), path.purpose).is_some() {
                return Err(AdminError::invalid("上游业务路径无效或重复"));
            }
        }
        if !paths
            .values()
            .any(|purpose| *purpose == UpstreamPathPurpose::Inference)
        {
            return Err(AdminError::invalid("上游适配器须声明推理路径"));
        }
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
        if query.len() > 64
            || query
                .iter()
                .any(|(key, value)| key.len() > 256 || value.len() > 4096)
        {
            return Err(invalid());
        }
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

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 2048
        && path.is_ascii()
        && !path.starts_with('/')
        && path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~')
        })
        && path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "upstream target is invalid")
}
