//! 跨 Provider 的标准化用量与单次请求总费用。

use std::fmt;
use std::str::FromStr;

use crate::error::AccountingError;

const DECIMAL_SCALE: u128 = 10_000_000_000;
const MAX_SCALED_DECIMAL: u128 = 99_999_999_999_999_999_999;

/// 与 PostgreSQL `numeric(20, 10)` 对齐的非负十进制定点值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Decimal(u128);

impl Decimal {
    pub const ZERO: Self = Self(0);

    /// 从按十位小数缩放的整数创建。
    ///
    /// # Errors
    ///
    /// 超出数据库范围时返回错误。
    pub const fn from_scaled(value: u128) -> Result<Self, AccountingError> {
        if value > MAX_SCALED_DECIMAL {
            return Err(AccountingError::InvalidDecimal);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn scaled(self) -> u128 {
        self.0
    }

    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0
            .checked_add(other.0)
            .filter(|value| *value <= MAX_SCALED_DECIMAL)
            .map(Self)
    }
}

impl FromStr for Decimal {
    type Err = AccountingError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || value.starts_with(['-', '+']) {
            return Err(AccountingError::InvalidDecimal);
        }
        let mut parts = value.split('.');
        let integer = parts.next().ok_or(AccountingError::InvalidDecimal)?;
        let fraction = parts.next().unwrap_or("");
        if parts.next().is_some()
            || integer.is_empty()
            || integer.len() > 10
            || !integer.bytes().all(|byte| byte.is_ascii_digit())
            || fraction.len() > 10
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(AccountingError::InvalidDecimal);
        }

        let integer = integer
            .parse::<u128>()
            .map_err(|_| AccountingError::InvalidDecimal)?;
        let fraction = if fraction.is_empty() {
            0
        } else {
            fraction
                .parse::<u128>()
                .map_err(|_| AccountingError::InvalidDecimal)?
                * 10_u128.pow(10_u32.saturating_sub(fraction.len() as u32))
        };
        let scaled = integer
            .checked_mul(DECIMAL_SCALE)
            .and_then(|whole| whole.checked_add(fraction))
            .ok_or(AccountingError::InvalidDecimal)?;
        Self::from_scaled(scaled)
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let integer = self.0 / DECIMAL_SCALE;
        let fraction = self.0 % DECIMAL_SCALE;
        write!(formatter, "{integer}.{fraction:010}")
    }
}

/// 三字符大写货币代码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CurrencyCode([u8; 3]);

impl CurrencyCode {
    /// 校验货币代码。
    ///
    /// # Errors
    ///
    /// 输入不是三个大写 ASCII 字符时返回错误。
    pub fn new(value: &str) -> Result<Self, AccountingError> {
        let bytes = value.as_bytes();
        if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_uppercase) {
            return Err(AccountingError::InvalidCurrency);
        }
        Ok(Self([bytes[0], bytes[1], bytes[2]]))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl fmt::Display for CurrencyCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 带货币的非负总金额。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Money {
    amount: Decimal,
    currency: CurrencyCode,
}

impl Money {
    #[must_use]
    pub const fn new(amount: Decimal, currency: CurrencyCode) -> Self {
        Self { amount, currency }
    }

    #[must_use]
    pub const fn amount(self) -> Decimal {
        self.amount
    }

    #[must_use]
    pub const fn currency(self) -> CurrencyCode {
        self.currency
    }

    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        if self.currency != other.currency {
            return None;
        }
        self.amount
            .checked_add(other.amount)
            .map(|amount| Self::new(amount, self.currency))
    }
}

/// `model_requests` 的六个公共 Token 事实。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    /// Provider/协议报告的独立事实，不从其他列相加推导。
    pub total_tokens: Option<u64>,
}

impl Usage {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
        }
    }

    /// 合并同一最终上游结果的增量观测；每个字段以较新的非空值为准。
    pub fn merge(&mut self, newer: &Self) {
        if newer.input_tokens.is_some() {
            self.input_tokens = newer.input_tokens;
        }
        if newer.output_tokens.is_some() {
            self.output_tokens = newer.output_tokens;
        }
        if newer.cached_tokens.is_some() {
            self.cached_tokens = newer.cached_tokens;
        }
        if newer.cache_write_tokens.is_some() {
            self.cache_write_tokens = newer.cache_write_tokens;
        }
        if newer.reasoning_tokens.is_some() {
            self.reasoning_tokens = newer.reasoning_tokens;
        }
        if newer.total_tokens.is_some() {
            self.total_tokens = newer.total_tokens;
        }
    }
}

/// 费用金额的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CostSource {
    ProviderReported,
    Calculated,
    Unavailable,
}

impl CostSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderReported => "provider_reported",
            Self::Calculated => "calculated",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Provider 或受控代码对当次请求总费用的可信程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CostEstimateStatus {
    Known,
    Partial,
    Unknown,
}

impl CostEstimateStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }
}

/// 单次模型请求的总费用，不保存价格版本或 breakdown。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostEstimate {
    status: CostEstimateStatus,
    source: CostSource,
    total: Option<Money>,
}

/// Provider 在单次请求终态上报的实际已计费总额。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProviderReportedCost {
    total: Money,
}

/// Provider 域依据公开单价和实际用量算出的单次总额。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CalculatedCost {
    total: Money,
}

/// Provider 受控价格规则计算出的运行时费用明细。
///
/// 该值只用于生成事件和管理端展示，不是持久化模型；数据库仍只保存最终总额、货币和来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalculatedCostBreakdown {
    input_amount: Money,
    output_amount: Money,
    cache_read_amount: Money,
    cache_write_amount: Money,
    standard_amount: Money,
    total_amount: Money,
    input_price_per_million: Money,
    output_price_per_million: Money,
    cache_read_price_per_million: Money,
    cache_write_price_per_million: Money,
    service_tier: Option<String>,
    multiplier_percent: u32,
}

/// 一次请求的费用组成，全部使用同一币种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalculatedCostAmounts {
    input: Money,
    output: Money,
    cache_read: Money,
    cache_write: Money,
    standard: Money,
    total: Money,
}

impl CalculatedCostAmounts {
    #[must_use]
    pub const fn new(
        input: Money,
        output: Money,
        cache_read: Money,
        cache_write: Money,
        standard: Money,
        total: Money,
    ) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
            standard,
            total,
        }
    }
}

/// 每百万 Token 的费率组成，全部使用同一币种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalculatedCostRates {
    input: Money,
    output: Money,
    cache_read: Money,
    cache_write: Money,
}

impl CalculatedCostRates {
    #[must_use]
    pub const fn new(input: Money, output: Money, cache_read: Money, cache_write: Money) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
        }
    }
}

impl CalculatedCostBreakdown {
    #[must_use]
    pub const fn new(
        amounts: CalculatedCostAmounts,
        rates: CalculatedCostRates,
        service_tier: Option<String>,
        multiplier_percent: u32,
    ) -> Self {
        Self {
            input_amount: amounts.input,
            output_amount: amounts.output,
            cache_read_amount: amounts.cache_read,
            cache_write_amount: amounts.cache_write,
            standard_amount: amounts.standard,
            total_amount: amounts.total,
            input_price_per_million: rates.input,
            output_price_per_million: rates.output,
            cache_read_price_per_million: rates.cache_read,
            cache_write_price_per_million: rates.cache_write,
            service_tier,
            multiplier_percent,
        }
    }

    #[must_use]
    pub const fn input_amount(&self) -> Money {
        self.input_amount
    }

    #[must_use]
    pub const fn output_amount(&self) -> Money {
        self.output_amount
    }

    #[must_use]
    pub const fn cache_read_amount(&self) -> Money {
        self.cache_read_amount
    }

    #[must_use]
    pub const fn cache_write_amount(&self) -> Money {
        self.cache_write_amount
    }

    #[must_use]
    pub const fn standard_amount(&self) -> Money {
        self.standard_amount
    }

    #[must_use]
    pub const fn total_amount(&self) -> Money {
        self.total_amount
    }

    #[must_use]
    pub const fn input_price_per_million(&self) -> Money {
        self.input_price_per_million
    }

    #[must_use]
    pub const fn output_price_per_million(&self) -> Money {
        self.output_price_per_million
    }

    #[must_use]
    pub const fn cache_read_price_per_million(&self) -> Money {
        self.cache_read_price_per_million
    }

    #[must_use]
    pub const fn cache_write_price_per_million(&self) -> Money {
        self.cache_write_price_per_million
    }

    #[must_use]
    pub fn service_tier(&self) -> Option<&str> {
        self.service_tier.as_deref()
    }

    #[must_use]
    pub const fn multiplier_percent(&self) -> u32 {
        self.multiplier_percent
    }

    #[must_use]
    pub const fn calculated_cost(&self) -> CalculatedCost {
        CalculatedCost {
            total: self.total_amount,
        }
    }
}

impl ProviderReportedCost {
    /// xAI 等 Provider 的 USD ticks 可直接传入；1 USD = 10^10 ticks。
    ///
    /// # Errors
    ///
    /// ticks 超出数据库 `numeric(20, 10)` 范围时失败。
    pub fn from_usd_ticks(ticks: u128) -> Result<Self, AccountingError> {
        Ok(Self {
            total: Money::new(Decimal::from_scaled(ticks)?, CurrencyCode(*b"USD")),
        })
    }

    #[must_use]
    pub const fn total(self) -> Money {
        self.total
    }

    #[must_use]
    pub const fn into_estimate(self) -> CostEstimate {
        CostEstimate {
            status: CostEstimateStatus::Known,
            source: CostSource::ProviderReported,
            total: Some(self.total),
        }
    }
}

impl CalculatedCost {
    /// 从精确 USD ticks 创建本地计算费用；1 USD = 10^10 ticks。
    ///
    /// # Errors
    ///
    /// ticks 超出数据库 `numeric(20, 10)` 范围时失败。
    pub fn from_usd_ticks(ticks: u128) -> Result<Self, AccountingError> {
        Ok(Self {
            total: Money::new(Decimal::from_scaled(ticks)?, CurrencyCode(*b"USD")),
        })
    }

    #[must_use]
    pub const fn total(self) -> Money {
        self.total
    }

    #[must_use]
    pub const fn into_estimate(self) -> CostEstimate {
        CostEstimate {
            status: CostEstimateStatus::Known,
            source: CostSource::Calculated,
            total: Some(self.total),
        }
    }
}

impl CostEstimate {
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            status: CostEstimateStatus::Unknown,
            source: CostSource::Unavailable,
            total: None,
        }
    }

    /// 创建有金额的 known/partial 结果。
    ///
    /// # Errors
    ///
    /// `Unavailable` 不能携带金额，`Unknown` 不能伪装成可信金额。
    pub fn priced(
        status: CostEstimateStatus,
        source: CostSource,
        total: Money,
    ) -> Result<Self, AccountingError> {
        if source == CostSource::Unavailable || status == CostEstimateStatus::Unknown {
            return Err(AccountingError::InvalidCostEstimate {
                status: status.as_str(),
            });
        }
        Ok(Self {
            status,
            source,
            total: Some(total),
        })
    }

    #[must_use]
    pub const fn status(&self) -> CostEstimateStatus {
        self.status
    }

    #[must_use]
    pub const fn source(&self) -> CostSource {
        self.source
    }

    #[must_use]
    pub const fn total(&self) -> Option<Money> {
        self.total
    }
}
