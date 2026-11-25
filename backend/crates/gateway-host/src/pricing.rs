//! models.dev 的只读价目适配；固定地址、禁止跳转，不进入推理请求链路

use async_trait::async_trait;
use gateway_admin::{
    model::{AdminError, pricing::PricingSyncPreview},
    ports::pricing::{PricingSource, PricingSourceScope},
};
use gateway_core::metering::{ModelPriceOverride, TokenPrice, TokenPriceOverride};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};

const SOURCE_URL: &str = "https://models.dev/api.json";
const MAX_BYTES: usize = 32 * 1024 * 1024;

pub struct ModelsDevPricing;

#[async_trait]
impl PricingSource for ModelsDevPricing {
    async fn fetch(&self, scope: &PricingSourceScope) -> Result<PricingSyncPreview, AdminError> {
        let client = gateway_egress::http_client(None)
            .map_err(|_| source_error())?
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
        decode_catalog(&body, scope)
    }
}

fn source_error() -> AdminError {
    AdminError::bad_gateway("无法读取 models.dev 价目，现有价格不变")
}

/// 读取文本参考价；外部模型如何对应网关模型由 Provider 登记
pub fn decode_catalog(
    body: &[u8],
    scope: &PricingSourceScope,
) -> Result<PricingSyncPreview, AdminError> {
    let catalog: Value = serde_json::from_slice(body).map_err(|_| source_error())?;
    let mut result = PricingSyncPreview {
        prices: BTreeMap::new(),
        skipped: Vec::new(),
    };
    for provider in scope.vendors() {
        let Some(models) = catalog
            .get(provider)
            .and_then(|p| p.get("models"))
            .and_then(Value::as_object)
        else {
            if scope.requires_catalog(provider) {
                return Err(source_error());
            }
            continue;
        };
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
    // Google 参考价只含文本单价，消费方须跳过无法据此拆分计费的音频请求
    if ["reasoning", "input_audio", "output_audio"]
        .iter()
        .any(|key| cost.get(key).is_some() && !(provider == "google" && *key == "input_audio"))
    {
        return None;
    }
    let long_context_threshold = if provider == "openai" {
        272_000
    } else {
        200_000
    };
    let mut bands = BTreeMap::from([("standard".to_owned(), rates(cost)?)]);
    if let Some(tiers) = cost.get("tiers") {
        let tiers = tiers.as_array()?;
        if tiers.len() > 1 {
            return None;
        }
        if let Some(tier) = tiers.first() {
            if tier.pointer("/tier/size")?.as_u64()? != long_context_threshold {
                return None;
            }
            bands.insert("long_standard".to_owned(), rates(tier)?);
        }
    } else if let Some(long) = cost.get("context_over_200k") {
        if long_context_threshold != 200_000 {
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
        // 没有缓存折扣的模型按普通输入价；不能把缺项解释成免费
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
