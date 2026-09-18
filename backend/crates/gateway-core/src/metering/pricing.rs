//! 请求冻结的本地价格覆盖；默认价目及模型别名继续由 Provider 拥有。

use std::collections::BTreeMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::Decimal;

/// 每百万 Token 的 USD 价格，保留四位小数以对齐单 Token 的金额精度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TokenPrice(Decimal);

impl TokenPrice {
    #[must_use]
    pub const fn ticks_per_token(self) -> u128 {
        self.0.scaled() / 1_000_000
    }
}

impl TryFrom<String> for TokenPrice {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let amount = Decimal::from_str(&value).map_err(|_| "单价必须为非负十进制字符串")?;
        if amount.scaled() % 1_000_000 != 0 || amount.scaled() > 1_000_000 * 10_000_000_000 {
            return Err("单价最多四位小数且不超过 1000000 USD / 百万 Token");
        }
        Ok(Self(amount))
    }
}

impl From<TokenPrice> for String {
    fn from(value: TokenPrice) -> Self {
        value.0.canonical()
    }
}

/// 一档完整 Token 单价；零价格与未覆盖的档位有不同含义。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenPriceOverride {
    pub input: TokenPrice,
    pub output: TokenPrice,
    pub cache_read: TokenPrice,
    pub cache_write: TokenPrice,
}

/// 一个精确模型的覆盖项；倍率使用基点，10000 表示 1 倍。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelPriceOverride {
    pub multiplier_bps: u32,
    pub bands: BTreeMap<String, TokenPriceOverride>,
}

impl ModelPriceOverride {
    /// 校验存储与管理入口共用的价格约束。
    ///
    /// # Errors
    /// 倍率超过 100 倍或档位不受支持时返回错误。
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.multiplier_bps > 1_000_000 {
            return Err("倍率必须在 0 至 100 倍之间");
        }
        if self.bands.keys().any(|band| {
            !matches!(
                band.as_str(),
                "standard"
                    | "fast"
                    | "flex"
                    | "long_standard"
                    | "long_fast"
                    | "long_flex"
                    | "image"
            )
        }) {
            return Err("价格档位不受支持");
        }
        Ok(())
    }
}

/// 按 Provider、上游模型索引的不可变覆盖事实。
pub type PricingOverrides = BTreeMap<String, BTreeMap<String, ModelPriceOverride>>;

/// 按档位叠加配置；倍率仅由人工覆盖控制，同步价目不携带业务倍率。
pub fn merge_pricing(mut base: PricingOverrides, overrides: &PricingOverrides) -> PricingOverrides {
    for (provider, models) in overrides {
        let target = base.entry(provider.clone()).or_default();
        for (model, pricing) in models {
            let entry = target
                .entry(model.clone())
                .or_insert_with(|| ModelPriceOverride {
                    multiplier_bps: 10_000,
                    bands: BTreeMap::new(),
                });
            entry.multiplier_bps = pricing.multiplier_bps;
            entry.bands.extend(pricing.bands.clone());
        }
    }
    base
}
