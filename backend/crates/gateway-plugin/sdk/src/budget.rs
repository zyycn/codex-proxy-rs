//! 按调用阶段解析有限处理预算，集中维护协议默认值与插件声明

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use crate::Stage;

/// 按阶段声明的毫秒预算；未声明的阶段采用协议默认值
pub type CallBudgets = BTreeMap<Stage, u64>;

const STAGES: [Stage; 18] = [
    Stage::Registration,
    Stage::Configuration,
    Stage::Authentication,
    Stage::Routing,
    Stage::Scheduling,
    Stage::Retry,
    Stage::Http,
    Stage::Service,
    Stage::WebSocket,
    Stage::Request,
    Stage::Attempt,
    Stage::Outbound,
    Stage::Upstream,
    Stage::Observation,
    Stage::Management,
    Stage::CommandLine,
    Stage::PublicManagement,
    Stage::Maintenance,
];

/// 实际预算来自协议默认值或插件显式声明
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallBudgetSource {
    Default,
    Plugin,
}

/// 已校验且不可变的会话预算，不允许构造缺少阶段的解析结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCallBudgets(BTreeMap<Stage, (Duration, CallBudgetSource)>);

impl ResolvedCallBudgets {
    /// 按协议默认值合并插件声明并冻结所有阶段的有效预算
    ///
    /// # Errors
    ///
    /// 声明值为零或平台计时器无法表示时失败
    pub fn resolve(declarations: &CallBudgets) -> Result<Self, CallBudgetError> {
        validate_values(declarations)?;
        let resolved = STAGES
            .into_iter()
            .map(|stage| {
                let (milliseconds, source) = match declarations.get(&stage) {
                    Some(&requested) => (requested, CallBudgetSource::Plugin),
                    None => (default_budget_ms(stage), CallBudgetSource::Default),
                };
                (stage, (Duration::from_millis(milliseconds), source))
            })
            .collect();
        Ok(Self(resolved))
    }

    /// 读取当前会话在指定阶段的处理预算
    #[must_use]
    pub fn budget(&self, stage: Stage) -> Duration {
        self.0[&stage].0
    }

    /// 读取预算来源，用于调用诊断
    #[must_use]
    pub fn source(&self, stage: Stage) -> CallBudgetSource {
        self.0[&stage].1
    }
}

/// 预算声明错误只暴露阶段和数值，不包含插件配置或业务数据
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallBudgetError {
    #[error("plugin call budget for {stage:?} is invalid: {milliseconds} ms")]
    Invalid { stage: Stage, milliseconds: u64 },
}

pub(crate) fn validate_values(values: &CallBudgets) -> Result<(), CallBudgetError> {
    for (&stage, &milliseconds) in values {
        if milliseconds == 0
            || Instant::now()
                .checked_add(Duration::from_millis(milliseconds))
                .is_none()
        {
            return Err(CallBudgetError::Invalid {
                stage,
                milliseconds,
            });
        }
    }
    Ok(())
}

const fn default_budget_ms(stage: Stage) -> u64 {
    match stage {
        Stage::Registration => 5_000,
        Stage::Configuration | Stage::Authentication => 30_000,
        Stage::Routing | Stage::Scheduling | Stage::Retry | Stage::Observation => 2_000,
        Stage::Http
        | Stage::Service
        | Stage::WebSocket
        | Stage::Request
        | Stage::Attempt
        | Stage::Outbound
        | Stage::Upstream
        | Stage::Management
        | Stage::CommandLine
        | Stage::PublicManagement
        | Stage::Maintenance => 120_000,
    }
}
