//! models.dev 的只读价目适配；固定地址、禁止跳转，不进入推理请求链路。

use async_trait::async_trait;
use gateway_admin::{
    model::{AdminError, pricing::PricingSyncPreview},
    ports::pricing::PricingSource,
};
use gateway_core::metering::{ModelPriceOverride, TokenPrice, TokenPriceOverride};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};

const SOURCE_URL: &str = "https://models.dev/api.json";
const MAX_BYTES: usize = 32 * 1024 * 1024;

pub struct ModelsDevPricing;

#[async_trait]
impl PricingSource for ModelsDevPricing {
    async fn fetch(&self) -> Result<PricingSyncPreview, AdminError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| source_error())?;
        let mut response = client
            .get(SOURCE_URL)
            .send()
            .await
            .map_err(|_| source_error())?
            .error_for_status()
            .map_err(|_| source_error())?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| source_error())? {
            if chunk.len() > MAX_BYTES.saturating_sub(body.len()) {
                return Err(source_error());
            }
            body.extend_from_slice(&chunk);
        }
        decode_catalog(&body)
    }
}

fn source_error() -> AdminError {
    AdminError::bad_gateway("无法读取 models.dev 价目，请稍后重试；现有价格未变更")
}

/// 只接受当前网关 Provider 的文本 Token 价格；不猜测缺失字段或模态换算。
pub fn decode_catalog(body: &[u8]) -> Result<PricingSyncPreview, AdminError> {
    let catalog: Value = serde_json::from_slice(body).map_err(|_| source_error())?;
    let mut result = PricingSyncPreview {
        prices: BTreeMap::new(),
        skipped: Vec::new(),
    };
    for provider in ["openai", "xai"] {
        let models = catalog
            .get(provider)
            .and_then(|p| p.get("models"))
            .and_then(Value::as_object)
            .ok_or_else(source_error)?;
        let mut prices = BTreeMap::new();
        for (model, value) in models {
            if model.is_empty()
                || model.len() > 128
                || model.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                continue;
            }
            match model_price(provider, value) {
                Some(price) => {
                    prices.insert(model.clone(), price);
                }
                None => result.skipped.push(format!("{provider}/{model}")),
            }
        }
        if !prices.is_empty() {
            result.prices.insert(provider.to_owned(), prices);
        }
    }
    if result.prices.is_empty() {
        return Err(source_error());
    }
    Ok(result)
}

fn model_price(provider: &str, model: &Value) -> Option<ModelPriceOverride> {
    let output = model.pointer("/modalities/output")?.as_array()?;
    if output.len() != 1 || output[0].as_str()? != "text" {
        return None;
    }
    let cost = model.get("cost")?;
    // 独立 reasoning/audio 价格无法用当前 Token 用量拆分，不能静默吞掉。
    if ["reasoning", "input_audio", "output_audio"]
        .iter()
        .any(|key| cost.get(key).is_some())
    {
        return None;
    }
    let mut bands = BTreeMap::from([("standard".to_owned(), rates(cost)?)]);
    if let Some(tiers) = cost.get("tiers") {
        let tiers = tiers.as_array()?;
        if tiers.len() > 1 {
            return None;
        }
        if let Some(tier) = tiers.first() {
            let expected = if provider == "xai" { 200_000 } else { 272_000 };
            if tier.pointer("/tier/size")?.as_u64()? != expected {
                return None;
            }
            bands.insert("long_standard".to_owned(), rates(tier)?);
        }
    } else if let Some(long) = cost.get("context_over_200k") {
        if provider != "xai" {
            return None;
        }
        bands.insert("long_standard".to_owned(), rates(long)?);
    }
    Some(ModelPriceOverride {
        multiplier_bps: 10_000,
        bands,
    })
}

fn rates(cost: &Value) -> Option<TokenPriceOverride> {
    let price = |key: &str| -> Option<TokenPrice> {
        cost.get(key)?.as_number()?.to_string().try_into().ok()
    };
    let input = price("input")?;
    Some(TokenPriceOverride {
        input,
        output: price("output")?,
        // 没有缓存折扣的模型按普通输入价；不能把缺项解释成免费。
        cache_read: if cost.get("cache_read").is_some() {
            price("cache_read")?
        } else {
            input
        },
        cache_write: if cost.get("cache_write").is_some() {
            price("cache_write")?
        } else {
            input
        },
    })
}
