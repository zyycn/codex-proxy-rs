//! 插件能力、调用阶段、失败策略与贡献声明的线协议定义

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Deserializer, Serialize, de};

/// 能力描述插件提供的处理器，不承担宿主资源授权
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    FrontendAuthentication,
    Scheduler,
    ModelRouter,
    ModelCatalog,
    RetryPolicy,
    Middleware,
    UpstreamAdapter,
    Observer,
    CommandLine,
    Management,
    Maintenance,
}

impl Capability {
    /// SDK 能描述的行为合同版本；具体宿主可以只开放其中一部分
    #[must_use]
    pub const fn contract_versions(self) -> &'static [u32] {
        match self {
            Self::Middleware => &[3, 4],
            Self::FrontendAuthentication
            | Self::Scheduler
            | Self::ModelRouter
            | Self::ModelCatalog
            | Self::RetryPolicy
            | Self::Observer
            | Self::CommandLine
            | Self::Management
            | Self::Maintenance => &[1],
            Self::UpstreamAdapter => &[1, 2],
        }
    }

    /// 稳定能力标识；默认扩展项 ID 由它派生，不受显示名称影响
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::FrontendAuthentication => "frontend_authentication",
            Self::Scheduler => "scheduler",
            Self::ModelRouter => "model_router",
            Self::ModelCatalog => "model_catalog",
            Self::RetryPolicy => "retry_policy",
            Self::Middleware => "middleware",
            Self::UpstreamAdapter => "upstream_adapter",
            Self::Observer => "observer",
            Self::CommandLine => "command_line",
            Self::Management => "management",
            Self::Maintenance => "maintenance",
        }
    }

    /// 固定调用阶段；只有中间件需要作者显式选择挂载边界
    #[must_use]
    pub const fn fixed_stages(self) -> &'static [Stage] {
        match self {
            Self::FrontendAuthentication => &[Stage::Authentication],
            Self::Scheduler => &[Stage::Scheduling],
            Self::ModelRouter => &[Stage::Routing],
            Self::ModelCatalog => &[Stage::Registration],
            Self::RetryPolicy => &[Stage::Retry],
            Self::Middleware => &[],
            Self::UpstreamAdapter => &[Stage::Upstream],
            Self::Observer => &[Stage::Observation],
            Self::CommandLine => &[Stage::CommandLine],
            Self::Management => &[Stage::Management],
            Self::Maintenance => &[Stage::Maintenance],
        }
    }
}

/// 调用阶段描述当前处理器位置，不限定插件访问宿主资源
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Registration,
    Configuration,
    Authentication,
    Routing,
    Scheduling,
    Retry,
    Http,
    Service,
    #[serde(rename = "websocket")]
    WebSocket,
    Request,
    Attempt,
    /// Core 登记已选账号的 attempt 后才启动的受管上游执行
    Upstream,
    Observation,
    Management,
    CommandLine,
    /// 未登录客户端触发的插件管理调用
    PublicManagement,
    /// 宿主针对已发布实例签发的幂等维护调用
    Maintenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    Reject,
    Delegate,
    Observe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContributionDeclaration {
    #[serde(default)]
    pub id: String,
    #[serde(default = "default_capability_version")]
    pub version: u32,
    #[serde(default)]
    pub stages: Vec<Stage>,
    #[serde(default)]
    pub input_formats: Vec<String>,
    #[serde(default)]
    pub output_formats: Vec<String>,
}

const fn default_capability_version() -> u32 {
    1
}

/// 插件按能力标识索引的扩展项声明；每种能力至多声明一个处理器
pub type Contributions = BTreeMap<Capability, ContributionDeclaration>;

pub(crate) fn deserialize_contributions<'de, D>(deserializer: D) -> Result<Contributions, D::Error>
where
    D: Deserializer<'de>,
{
    struct ContributionsVisitor;

    impl<'de> de::Visitor<'de> for ContributionsVisitor {
        type Value = Contributions;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map with one declaration per plugin capability")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: de::MapAccess<'de>,
        {
            let mut contributions = Contributions::new();
            while let Some((capability, declaration)) = map.next_entry()? {
                if contributions.insert(capability, declaration).is_some() {
                    return Err(de::Error::custom("duplicate plugin capability declaration"));
                }
            }
            Ok(contributions)
        }
    }

    deserializer.deserialize_map(ContributionsVisitor)
}
