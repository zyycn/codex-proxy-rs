//! Kimi、DeepSeek 与 Sub2API 余额及套餐额度查询、响应解释和凭据版本隔离

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use gateway_admin::model::account_balance::{
    AccountBalance, AccountBalanceDetail, AccountBalanceEntry,
};
use gateway_admin::model::provider_credentials::{
    ProviderQuota, ProviderQuotaWindow, QuotaLocalUsageAttribution,
};
use gateway_admin::ports::provider::{ProviderAdminError, ProviderAdminErrorKind};
use gateway_core::account::{
    OpaqueProviderData, ProviderAccountId, ProviderAccountStore, QuotaObservation,
    QuotaWriteOutcome,
};
use serde_json::{Number, Value};

use crate::credential::{ApiKeyBalanceQuery, CodexCredentialCodec, CodexCredentialData};

const MAX_RESPONSE_BYTES: usize = 256 * 1024;

pub(super) fn project_quota(
    balance: Option<AccountBalance>,
) -> Result<ProviderQuota, ProviderAdminError> {
    let Some(balance) = balance else {
        return Ok(super::empty_quota());
    };
    let windows = balance
        .entries
        .into_iter()
        .enumerate()
        .filter(|(_, entry)| entry.unit == "%" && entry.is_valid)
        .map(|(index, entry)| {
            let used_percent = entry
                .used
                .as_deref()
                .ok_or_else(invalid_response)?
                .parse::<f64>()
                .map_err(|_| invalid_response())?;
            Ok(ProviderQuotaWindow {
                key: format!("api_key_balance:{index}"),
                group: if entry.window_seconds.is_some() {
                    "shortTerm"
                } else {
                    "other"
                }
                .to_owned(),
                label: entry.label,
                limit_id: Some("api_key_balance".to_owned()),
                limit_name: None,
                role: None,
                local_usage_attribution: if entry.window_seconds.is_some() {
                    QuotaLocalUsageAttribution::AccountWide
                } else {
                    QuotaLocalUsageAttribution::Unavailable
                },
                window_seconds: entry.window_seconds,
                used_percent: Some(used_percent),
                reset_at: entry.reset_at,
                limit_reached: used_percent >= 100.0,
                local_usage: None,
                provider_data: None,
            })
        })
        .collect::<Result<_, ProviderAdminError>>()?;
    Ok(ProviderQuota {
        observed_at: balance.observed_at,
        windows,
        ..super::empty_quota()
    })
}

fn error(message: &'static str) -> ProviderAdminError {
    ProviderAdminError::new(ProviderAdminErrorKind::BadGateway).with_public_message(message)
}

fn invalid_response() -> ProviderAdminError {
    error("上游余额响应格式不符合所选查询类型")
}

pub(crate) async fn account_balance(
    store: &dyn ProviderAccountStore,
    account_id: &ProviderAccountId,
    refresh: bool,
) -> Result<Option<AccountBalance>, ProviderAdminError> {
    let current = store
        .load_current_credential(account_id)
        .await
        .map_err(super::map_store_error)?;
    let CodexCredentialData::ApiKey(data) =
        CodexCredentialCodec::decode_complete(&current.credential)
            .map_err(|_| ProviderAdminError::new(ProviderAdminErrorKind::Invalid))?
    else {
        return Err(ProviderAdminError::new(ProviderAdminErrorKind::Unsupported));
    };
    if data.balance_query == ApiKeyBalanceQuery::Disabled {
        return Ok(None);
    }
    let account = current.account;
    if !refresh {
        let stored = store
            .get_quotas(std::slice::from_ref(account_id))
            .await
            .map_err(super::map_store_error)?
            .into_iter()
            .find(|observation| observation.expected_revision == account.revision());
        if let Some(stored) = stored
            && stored
                .quota
                .expose_to_provider()
                .get("credential_revision")
                .and_then(Value::as_u64)
                == Some(account.revision().get())
            && let Some(value) = stored.quota.expose_to_provider().get("api_key_balance")
        {
            let balance: AccountBalance =
                serde_json::from_value(value.clone()).map_err(|_| invalid_response())?;
            if balance.query_type == data.balance_query.as_str() {
                return Ok(Some(balance));
            }
        }
        return Ok(Some(AccountBalance {
            query_type: data.balance_query.as_str().to_owned(),
            observed_at: None,
            entries: Vec::new(),
        }));
    }

    // 查询开始时间防止较早发出的慢请求覆盖较晚发出的结果
    let observed_at = SystemTime::now();
    let url = query_url(&data.base_url, data.balance_query)?;
    let client = crate::transport::client::build_account_http_client(
        account_id.as_str(),
        account.outbound_proxy(),
    )
    .map_err(|_| error("无法创建账号余额查询连接"))?;
    let mut response = client
        .get(url.as_str())
        .bearer_auth(&data.api_key)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(
            if matches!(
                data.balance_query,
                ApiKeyBalanceQuery::KimiCoding | ApiKeyBalanceQuery::KimiCodingGlobal
            ) {
                15
            } else {
                10
            },
        ))
        .send()
        .await
        .map_err(|failure| {
            if failure.is_timeout() {
                error("上游余额查询超时")
            } else {
                error("无法连接上游用量接口，请检查查询设置和账号出口")
            }
        })?;
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            401 | 403 => error("余额查询被拒绝，请检查 API Key 和查询类型"),
            404 => error("上游没有所选用量接口，请检查查询设置"),
            429 => error("上游余额查询过于频繁，请稍后重试"),
            _ => error("上游余额接口暂时不可用"),
        });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error("读取上游余额响应失败"))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(error("上游余额响应超过大小限制"));
        }
        body.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&body).map_err(|_| invalid_response())?;
    let entries = match data.balance_query {
        ApiKeyBalanceQuery::Kimi => parse_kimi(&value, "CNY")?,
        ApiKeyBalanceQuery::KimiGlobal => parse_kimi(&value, "USD")?,
        ApiKeyBalanceQuery::KimiCoding | ApiKeyBalanceQuery::KimiCodingGlobal => {
            parse_kimi_coding(&value)?
        }
        ApiKeyBalanceQuery::Deepseek => parse_deepseek(&value)?,
        ApiKeyBalanceQuery::Sub2api => parse_sub2api(&value)?,
        ApiKeyBalanceQuery::Disabled => unreachable!(),
    };
    let balance = AccountBalance {
        query_type: data.balance_query.as_str().to_owned(),
        observed_at: Some(DateTime::<Utc>::from(observed_at)),
        entries,
    };
    let document = serde_json::json!({
        "credential_revision": account.revision().get(),
        "api_key_balance": &balance,
    });
    let outcome = store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account_id.clone(),
            expected_revision: account.revision(),
            quota: OpaqueProviderData::new(document.as_object().cloned().expect("余额文档为对象")),
            plan_type: None,
            observed_at,
            // 余额查询不提供推理可用性的授权证据，保留现有访问事实及其时间
            state: account.quota(),
        })
        .await
        .map_err(super::map_store_error)?;
    if outcome == QuotaWriteOutcome::Conflict {
        return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict)
            .with_public_message("账号设置或余额已更新，请重新查询"));
    }
    Ok(Some(balance))
}

fn query_url(base_url: &str, query: ApiKeyBalanceQuery) -> Result<url::Url, ProviderAdminError> {
    let (base_url, suffix) = match query {
        ApiKeyBalanceQuery::Kimi => ("https://api.moonshot.cn", "/v1/users/me/balance"),
        ApiKeyBalanceQuery::KimiGlobal => ("https://api.moonshot.ai", "/v1/users/me/balance"),
        ApiKeyBalanceQuery::KimiCoding => ("https://api.kimi.com", "/coding/v1/usages"),
        ApiKeyBalanceQuery::KimiCodingGlobal => ("https://api.kimi.ai", "/coding/v1/usages"),
        ApiKeyBalanceQuery::Deepseek => (base_url, "/user/balance"),
        ApiKeyBalanceQuery::Sub2api => (base_url, "/v1/usage"),
        ApiKeyBalanceQuery::Disabled => unreachable!(),
    };
    let mut url =
        crate::transport::parse_upstream_base_url(base_url).ok_or_else(invalid_response)?;
    let path = url.path().trim_end_matches('/');
    let root = path.strip_suffix("/v1").unwrap_or(path);
    url.set_path(&format!("{root}{suffix}"));
    Ok(url)
}

fn amount(value: &Value) -> Result<String, ProviderAdminError> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => return Err(invalid_response()),
    };
    if text.len() > 128 || text.parse::<Number>().is_err() {
        return Err(invalid_response());
    }
    Ok(text)
}

fn optional_amount(value: &Value) -> Result<Option<String>, ProviderAdminError> {
    if value.is_null() {
        Ok(None)
    } else {
        amount(value).map(Some)
    }
}

fn entry(label: &str, unit: &str) -> AccountBalanceEntry {
    AccountBalanceEntry {
        label: label.to_owned(),
        remaining: None,
        used: None,
        total: None,
        unit: unit.to_owned(),
        unlimited: false,
        is_valid: true,
        invalid_message: None,
        window_seconds: None,
        reset_at: None,
        details: Vec::new(),
    }
}

fn parse_kimi(value: &Value, unit: &str) -> Result<Vec<AccountBalanceEntry>, ProviderAdminError> {
    if value["status"] != true || value["code"] != 0 {
        return Err(invalid_response());
    }
    let mut result = entry("可用余额", unit);
    result.remaining = Some(amount(&value["data"]["available_balance"])?);
    for (key, label) in [
        ("cash_balance", "现金余额"),
        ("voucher_balance", "代金券余额"),
    ] {
        result.details.push(AccountBalanceDetail {
            label: label.to_owned(),
            amount: amount(&value["data"][key])?,
        });
    }
    Ok(vec![result])
}

fn parse_kimi_coding(value: &Value) -> Result<Vec<AccountBalanceEntry>, ProviderAdminError> {
    let mut entries = Vec::new();
    // 与 CC Switch 相同，先展示短周期，再展示订阅周额度
    if let Some(limits) = value["limits"].as_array() {
        for limit in limits {
            if let Some(detail) = limit.get("detail") {
                entries.push(kimi_quota_entry(detail, "5 小时额度", 5 * 60 * 60)?);
            }
        }
    }
    if let Some(usage) = value.get("usage") {
        entries.push(kimi_quota_entry(usage, "周额度", 7 * 24 * 60 * 60)?);
    }
    if entries.is_empty() {
        return Err(invalid_response());
    }
    Ok(entries)
}

fn kimi_quota_entry(
    detail: &Value,
    label: &str,
    window_seconds: u64,
) -> Result<AccountBalanceEntry, ProviderAdminError> {
    let limit = kimi_quota_number(&detail["limit"])?;
    if limit <= 0.0 {
        return Err(invalid_response());
    }
    let used = if detail["remaining"].is_null() {
        // 上游也会只返回 used，不能将缺失的 remaining 当成零剩余
        kimi_quota_number(&detail["used"])?
    } else {
        limit - kimi_quota_number(&detail["remaining"])?
    };
    let used_percent = (used.max(0.0) / limit * 100.0).clamp(0.0, 100.0);
    let mut result = entry(label, "%");
    result.remaining = Some(kimi_percentage(100.0 - used_percent));
    result.used = Some(kimi_percentage(used_percent));
    result.total = Some("100".to_owned());
    result.window_seconds = Some(window_seconds);
    result.reset_at = kimi_reset_at(&detail["resetTime"])?;
    Ok(result)
}

fn kimi_quota_number(value: &Value) -> Result<f64, ProviderAdminError> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .filter(|number| number.is_finite())
        .ok_or_else(invalid_response)
}

fn kimi_percentage(value: f64) -> String {
    format!("{value:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn kimi_reset_at(value: &Value) -> Result<Option<DateTime<Utc>>, ProviderAdminError> {
    if value.is_null() {
        return Ok(None);
    }
    if let Some(text) = value.as_str() {
        return DateTime::parse_from_rfc3339(text)
            .map(|time| Some(time.with_timezone(&Utc)))
            .map_err(|_| invalid_response());
    }
    let timestamp = value.as_i64().ok_or_else(invalid_response)?;
    if timestamp <= 0 {
        return Ok(None);
    }
    // 沿用 CC Switch 的秒、毫秒时间戳分界
    let time = if timestamp < 1_000_000_000_000 {
        DateTime::from_timestamp(timestamp, 0)
    } else {
        DateTime::from_timestamp_millis(timestamp)
    };
    time.map(Some).ok_or_else(invalid_response)
}

fn parse_deepseek(value: &Value) -> Result<Vec<AccountBalanceEntry>, ProviderAdminError> {
    let available = value["is_available"]
        .as_bool()
        .ok_or_else(invalid_response)?;
    let infos = value["balance_infos"]
        .as_array()
        .ok_or_else(invalid_response)?;
    if infos.is_empty() {
        return Err(invalid_response());
    }
    infos
        .iter()
        .map(|info| {
            let currency = info["currency"].as_str().ok_or_else(invalid_response)?;
            if !matches!(currency, "CNY" | "USD") {
                return Err(invalid_response());
            }
            let mut result = entry("可用余额", currency);
            result.remaining = Some(amount(&info["total_balance"])?);
            result.is_valid = available;
            result.invalid_message = (!available).then(|| "余额不足".to_owned());
            for (key, label) in [
                ("granted_balance", "赠送余额"),
                ("topped_up_balance", "充值余额"),
            ] {
                result.details.push(AccountBalanceDetail {
                    label: label.to_owned(),
                    amount: amount(&info[key])?,
                });
            }
            Ok(result)
        })
        .collect()
}

fn parse_sub2api(value: &Value) -> Result<Vec<AccountBalanceEntry>, ProviderAdminError> {
    // Sub2API 的 CC Switch 模板允许缺少 isValid，明确 false 时才显示无效
    if !value.is_object() || !value["error"].is_null() {
        return Err(invalid_response());
    }
    let valid = value["isValid"].as_bool().unwrap_or(true);
    let unit = value["unit"]
        .as_str()
        .or_else(|| value["quota"]["unit"].as_str())
        .unwrap_or("USD");
    let mut result = entry(value["planName"].as_str().unwrap_or("上游额度"), unit);
    result.is_valid = valid;
    result.invalid_message = (!valid).then(|| "上游 Key 不可用".to_owned());
    let remaining = optional_amount(&value["remaining"])?;
    result.unlimited = value["subscription"].is_object() && remaining.as_deref() == Some("-1");
    result.remaining = remaining.filter(|_| !result.unlimited);
    if value["quota"]["limit"]
        .as_f64()
        .is_some_and(|limit| limit > 0.0)
    {
        result.label = "Key 总额度".to_owned();
        result.used = Some(amount(&value["quota"]["used"])?);
        result.total = Some(amount(&value["quota"]["limit"])?);
    }
    let mut entries = vec![result];
    if value["subscription"].is_object() {
        for (prefix, label) in [
            ("daily", "订阅日额度"),
            ("weekly", "订阅周额度"),
            ("monthly", "订阅月额度"),
        ] {
            let limit = &value["subscription"][format!("{prefix}_limit_usd")];
            if limit.as_f64().is_some_and(|limit| limit > 0.0) {
                let mut window = entry(label, unit);
                window.total = Some(amount(limit)?);
                window.used = Some(amount(
                    &value["subscription"][format!("{prefix}_usage_usd")],
                )?);
                entries.push(window);
            }
        }
    }
    if let Some(windows) = value["rate_limits"].as_array() {
        for window in windows {
            let label = match window["window"].as_str() {
                Some("5h") => "Key 5 小时额度",
                Some("1d") => "Key 日额度",
                Some("7d") => "Key 周额度",
                _ => return Err(invalid_response()),
            };
            let mut item = entry(label, unit);
            item.remaining = Some(amount(&window["remaining"])?);
            item.used = Some(amount(&window["used"])?);
            item.total = Some(amount(&window["limit"])?);
            item.reset_at = window["reset_at"]
                .as_str()
                .map(|time| DateTime::parse_from_rfc3339(time).map(|time| time.with_timezone(&Utc)))
                .transpose()
                .map_err(|_| invalid_response())?;
            entries.push(item);
        }
    }
    if entries[0].remaining.is_none()
        && entries[0].total.is_none()
        && !entries[0].unlimited
        && valid
        && value["planName"].as_str().is_none()
    {
        if entries.len() == 1 {
            return Err(invalid_response());
        }
        entries.remove(0);
    }
    Ok(entries)
}
