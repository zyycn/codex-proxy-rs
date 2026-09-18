//! 价格覆盖与请求费用明细的持久化。

use gateway_admin::model::pricing::{PricingChange, UpdatePricing};
use gateway_core::metering::{ModelPriceOverride, PricingOverrides};
use sqlx::types::Json;

use super::{
    AdminAuditEvent, PgControlPlaneRepository, append_admin_audit_event_in_transaction,
    bump_config_revision_in_transaction,
};
use crate::{Revision, StoreError, StoreResult, postgres_unavailable};

pub(crate) fn encode_billing_snapshot(
    b: &gateway_core::metering::CalculatedCostBreakdown,
) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "image": b.image().map(|image| serde_json::json!({
            "inputTokens": image.input_tokens, "cachedTokens": image.cached_tokens,
            "input": image.input_amount.amount().canonical(),
            "cacheRead": image.cache_read_amount.amount().canonical(),
            "inputPrice": image.input_price_per_million.amount().canonical(),
            "cacheReadPrice": image.cache_read_price_per_million.amount().canonical(),
        })),
        "input": b.input_amount().amount().canonical(),
        "output": b.output_amount().amount().canonical(),
        "cacheRead": b.cache_read_amount().amount().canonical(),
        "cacheWrite": b.cache_write_amount().amount().canonical(),
        "standard": b.standard_amount().amount().canonical(),
        "total": b.total_amount().amount().canonical(),
        "inputPrice": b.input_price_per_million().amount().canonical(),
        "outputPrice": b.output_price_per_million().amount().canonical(),
        "cacheReadPrice": b.cache_read_price_per_million().amount().canonical(),
        "cacheWritePrice": b.cache_write_price_per_million().amount().canonical(),
        "currency": b.total_amount().currency().as_str(),
        "serviceTier": b.service_tier(),
        "multiplierPercent": b.multiplier_percent(),
        "customMultiplierBps": b.custom_multiplier_bps(),
    })
}

pub(crate) fn decode_billing_snapshot(
    value: &serde_json::Value,
) -> Option<gateway_admin::model::observability::CalculatedBillingBreakdown> {
    use gateway_admin::model::observability::{CalculatedBillingBreakdown, CurrencyCost};
    if value.get("version")?.as_u64()? != 1 {
        return None;
    }
    let currency = value.get("currency")?.as_str()?;
    gateway_core::metering::CurrencyCode::new(currency).ok()?;
    let amount = |key| {
        Some(CurrencyCost {
            currency: currency.to_owned(),
            amount: value.get(key)?.as_str()?.parse().ok()?,
        })
    };
    Some(CalculatedBillingBreakdown {
        image: match value.get("image").filter(|v| !v.is_null()) {
            Some(image) => {
                let amount = |key| {
                    Some(CurrencyCost {
                        currency: currency.to_owned(),
                        amount: image.get(key)?.as_str()?.parse().ok()?,
                    })
                };
                Some(gateway_admin::model::observability::ImageBillingBreakdown {
                    input_tokens: image.get("inputTokens")?.as_u64()?,
                    cached_tokens: image.get("cachedTokens")?.as_u64()?,
                    input_amount: amount("input")?,
                    cache_read_amount: amount("cacheRead")?,
                    input_price_per_million: amount("inputPrice")?,
                    cache_read_price_per_million: amount("cacheReadPrice")?,
                })
            }
            None => None,
        },
        input_amount: amount("input")?,
        output_amount: amount("output")?,
        cache_read_amount: amount("cacheRead")?,
        cache_write_amount: amount("cacheWrite")?,
        standard_amount: amount("standard")?,
        total_amount: amount("total")?,
        input_price_per_million: amount("inputPrice")?,
        output_price_per_million: amount("outputPrice")?,
        cache_read_price_per_million: amount("cacheReadPrice")?,
        cache_write_price_per_million: amount("cacheWritePrice")?,
        service_tier: value.get("serviceTier")?.as_str().map(str::to_owned),
        multiplier_percent: value.get("multiplierPercent")?.as_u64()?.try_into().ok()?,
        custom_multiplier_bps: value
            .get("customMultiplierBps")?
            .as_u64()?
            .try_into()
            .ok()?,
    })
}

pub(crate) fn validate_pricing(pricing: &PricingOverrides) -> StoreResult<()> {
    if pricing
        .values()
        .map(std::collections::BTreeMap::len)
        .sum::<usize>()
        > 10_000
    {
        return Err(invalid_pricing());
    }
    for (provider, models) in pricing {
        if !matches!(provider.as_str(), "openai" | "xai") {
            return Err(invalid_pricing());
        }
        for (model, pricing) in models {
            if model.is_empty()
                || model.len() > 128
                || model.chars().any(char::is_whitespace)
                || model.chars().any(char::is_control)
                || pricing.validate().is_err()
            {
                return Err(invalid_pricing());
            }
        }
    }
    Ok(())
}

fn invalid_pricing() -> StoreError {
    StoreError::InvalidData {
        entity: "model pricing",
        message: "invalid model pricing".to_owned(),
    }
}

impl PgControlPlaneRepository {
    pub(crate) async fn load_pricing(
        &self,
    ) -> StoreResult<gateway_admin::model::pricing::StoredPricing> {
        let (Json(overrides), Json(synced), synced_at) = sqlx::query_as::<_, (Json<PricingOverrides>, Json<PricingOverrides>, Option<chrono::DateTime<chrono::Utc>>)>(
            "select pricing_overrides_json, pricing_synced_json, pricing_synced_at from runtime_settings where id = 1")
            .fetch_one(&self.pool).await.map_err(|_| postgres_unavailable("load model pricing"))?;
        validate_pricing(&overrides)?;
        validate_pricing(&synced)?;
        Ok(gateway_admin::model::pricing::StoredPricing {
            overrides,
            synced,
            synced_at,
        })
    }

    pub(crate) async fn sync_pricing(
        &self,
        prices: PricingOverrides,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        validate_pricing(&prices)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin pricing sync"))?;
        // 同步只替换来源层；不写人工覆盖。运行设置行锁统一串行化配置修订。
        sqlx::query("update runtime_settings set pricing_synced_json = $1, pricing_synced_at = now() where id = 1")
            .bind(Json(prices)).execute(&mut *transaction).await.map_err(|_| postgres_unavailable("sync model pricing"))?;
        let revision = bump_config_revision_in_transaction(&mut transaction).await?;
        append_admin_audit_event_in_transaction(&mut transaction, audit, revision).await?;
        transaction
            .commit()
            .await
            .map_err(|_| postgres_unavailable("commit pricing sync"))?;
        Ok(revision)
    }

    pub(crate) async fn update_pricing(
        &self,
        command: UpdatePricing,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin model pricing update"))?;
        // 锁定当前配置再更新选中项，避免批量操作覆盖其他管理员已提交的模型。
        let Json(mut pricing) = sqlx::query_scalar::<_, Json<PricingOverrides>>(
            "select pricing_overrides_json from runtime_settings where id = 1 for update",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| postgres_unavailable("lock model pricing"))?;
        let models = pricing.entry(command.provider).or_default();
        for model in command.models {
            match &command.change {
                PricingChange::Reset => {
                    models.remove(&model);
                }
                PricingChange::Replace(value) => {
                    models.insert(model, value.clone());
                }
                PricingChange::Multiplier(bps) => {
                    models
                        .entry(model)
                        .or_insert_with(|| ModelPriceOverride {
                            multiplier_bps: 10_000,
                            bands: Default::default(),
                        })
                        .multiplier_bps = *bps;
                }
            }
        }
        pricing.retain(|_, models| !models.is_empty());
        validate_pricing(&pricing)?;
        sqlx::query("update runtime_settings set pricing_overrides_json = $1 where id = 1")
            .bind(Json(pricing))
            .execute(&mut *transaction)
            .await
            .map_err(|_| postgres_unavailable("update model pricing"))?;
        let revision = bump_config_revision_in_transaction(&mut transaction).await?;
        append_admin_audit_event_in_transaction(&mut transaction, audit, revision).await?;
        transaction
            .commit()
            .await
            .map_err(|_| postgres_unavailable("commit model pricing"))?;
        Ok(revision)
    }
}
