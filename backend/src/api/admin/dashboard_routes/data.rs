use super::*;

pub(super) fn dashboard_cards(
    accounts: &[Account],
    buckets: &[AccountUsageTimeBucket],
    usage_summary: &UsageRecordSummary,
) -> DashboardCardsData {
    dashboard_cards_at(accounts, buckets, usage_summary, Utc::now())
}

pub(super) fn dashboard_cards_at(
    accounts: &[Account],
    buckets: &[AccountUsageTimeBucket],
    usage_summary: &UsageRecordSummary,
    now: DateTime<Utc>,
) -> DashboardCardsData {
    let today_start = china_day_start(now);
    let yesterday_start = today_start - Duration::days(1);
    let today = usage_window(buckets, today_start, now);
    let yesterday = usage_window(buckets, yesterday_start, today_start);
    let today_cost = cost_window(buckets, today_start, now).unwrap_or(0.0);
    let total_requests = usage_summary.total_requests;
    let total_tokens = usage_summary.total_tokens;
    let total_cached_tokens = usage_summary.cached_tokens;
    let total_hit_rate = summary_cache_hit_rate(usage_summary);

    let total_accounts = accounts.len() as u64;
    let enabled_accounts = accounts
        .iter()
        .filter(|account| account.status == AccountStatus::Active)
        .count() as u64;
    let abnormal_accounts = accounts
        .iter()
        .filter(|account| {
            matches!(
                account.status,
                AccountStatus::Expired
                    | AccountStatus::QuotaExhausted
                    | AccountStatus::Disabled
                    | AccountStatus::Banned
            )
        })
        .count() as u64;

    DashboardCardsData {
        accounts: DashboardAccountsCardData {
            total: format_compact_number(total_accounts),
            total_value: total_accounts,
            enabled: format_compact_number(enabled_accounts),
            enabled_value: enabled_accounts,
            abnormal: format_compact_number(abnormal_accounts),
            abnormal_value: abnormal_accounts,
        },
        traffic: DashboardTrafficCardData {
            today_requests: format_compact_number(today.requests),
            today_requests_value: today.requests,
            yesterday_requests_value: yesterday.requests,
            total_requests: format_compact_number(total_requests),
        },
        tokens: DashboardTokenCardData {
            today_tokens: format_tokens(today.tokens()),
            today_tokens_value: today.tokens(),
            yesterday_tokens_value: yesterday.tokens(),
            total_tokens: format_tokens(total_tokens),
            today_cost_usd: format_cost(today_cost),
        },
        cache: DashboardCacheCardData {
            today_hit_rate: format_rate(today.cache_hit_rate()),
            today_hit_rate_value: today.cache_hit_rate(),
            yesterday_hit_rate_value: yesterday.cache_hit_rate(),
            total_hit_rate: format_rate(total_hit_rate),
            total_cached_tokens: format_tokens(total_cached_tokens),
            average_first_token_latency_ms: format_optional_duration_ms(
                today.avg_first_token_latency(),
            ),
        },
    }
}

pub(super) fn summary_cache_hit_rate(summary: &UsageRecordSummary) -> Option<f64> {
    let input_tokens = summary.input_tokens;
    if input_tokens > 0 {
        Some(summary.cached_tokens as f64 / input_tokens as f64)
    } else {
        None
    }
}

pub(super) fn format_optional_duration_ms(value: Option<u64>) -> String {
    format_duration_ms(value.and_then(|value| i64::try_from(value).ok()))
}

pub(super) async fn dashboard_time_buckets(
    state: &AppState,
    now: DateTime<Utc>,
) -> Vec<AccountUsageTimeBucket> {
    let current_slot = china_quarter_hour_start(now);
    let start = current_slot
        - Duration::minutes(HEALTH_TIMELINE_SLOT_MINUTES * (DASHBOARD_TIME_BUCKET_SLOTS - 1));
    state
        .services
        .usage
        .time_buckets(start, now)
        .await
        .unwrap_or_default()
}

pub(super) fn dashboard_trend_data(
    buckets: &[AccountUsageTimeBucket],
    kind: DashboardTrendKind,
) -> DashboardTrendData {
    dashboard_trend_data_at(buckets, kind, Utc::now())
}

pub(super) fn dashboard_trend_data_at(
    records: &[AccountUsageTimeBucket],
    kind: DashboardTrendKind,
    now: DateTime<Utc>,
) -> DashboardTrendData {
    let current_hour = china_hour_start(now);
    let start = china_day_start(now);
    let elapsed_hours = current_hour.signed_duration_since(start).num_hours();
    let mut buckets = (0..=elapsed_hours)
        .map(|index| {
            let bucket_start = start + Duration::hours(index);
            (bucket_start, UsageWindow::default())
        })
        .collect::<Vec<_>>();

    for record in records {
        if record.bucket_start < start || record.bucket_start > now {
            continue;
        }
        let record_hour = china_hour_start(record.bucket_start);
        if let Some((_, bucket)) = buckets
            .iter_mut()
            .find(|(bucket_start, _)| *bucket_start == record_hour)
        {
            apply_bucket(bucket, record);
        }
    }

    let points = buckets
        .iter()
        .map(|(bucket_start, bucket)| {
            let latency = bucket.avg_first_token_latency().unwrap_or(0);
            let max_latency = bucket.max_first_token_bucket_latency().unwrap_or(0);
            let min_latency = bucket.min_first_token_bucket_latency().unwrap_or(0);
            let tokens = bucket.tokens();
            let cache_hit_rate = bucket.cache_hit_rate().unwrap_or(0.0);
            let success_rate = if bucket.requests > 0 {
                ((bucket.requests - bucket.errors) as f64 / bucket.requests as f64 * 1000.0).round()
                    / 10.0
            } else {
                0.0
            };
            DashboardTrendPointData {
                time: format!("{:02}", china_hour(bucket_start)),
                requests: format_compact_number(bucket.requests),
                requests_value: bucket.requests,
                input_tokens: format_tokens(bucket.input_tokens),
                input_tokens_value: bucket.input_tokens,
                output_tokens: format_tokens(bucket.output_tokens),
                output_tokens_value: bucket.output_tokens,
                cached_tokens: format_tokens(bucket.cached_tokens),
                cached_tokens_value: bucket.cached_tokens,
                cache_hit_rate_value: cache_hit_rate,
                tokens_value: tokens,
                errors: format_compact_number(bucket.errors),
                errors_value: bucket.errors,
                latency: format_optional_duration_ms(Some(latency)),
                latency_value: latency,
                max_latency: format_optional_duration_ms(Some(max_latency)),
                max_latency_value: max_latency,
                min_latency: format_optional_duration_ms(Some(min_latency)),
                min_latency_value: min_latency,
                success_rate: format_percent(success_rate),
                success_rate_value: success_rate,
            }
        })
        .collect::<Vec<_>>();

    DashboardTrendData {
        kind,
        summary: trend_summary(kind, &points),
        points,
    }
}

pub(super) fn dashboard_health_timeline_data(
    buckets: &[AccountUsageTimeBucket],
) -> DashboardHealthTimelineData {
    dashboard_health_timeline_data_at(buckets, Utc::now())
}

pub(super) fn dashboard_health_timeline_data_at(
    records: &[AccountUsageTimeBucket],
    now: DateTime<Utc>,
) -> DashboardHealthTimelineData {
    let current_slot = china_quarter_hour_start(now);
    let start = china_day_start(now);
    let mut buckets = (0..HEALTH_TIMELINE_SLOTS)
        .map(|index| {
            let bucket_start = start + Duration::minutes(HEALTH_TIMELINE_SLOT_MINUTES * index);
            (bucket_start, UsageWindow::default())
        })
        .collect::<Vec<_>>();

    for record in records {
        if record.bucket_start < start || record.bucket_start > now {
            continue;
        }
        let record_slot = china_quarter_hour_start(record.bucket_start);
        if let Some((_, bucket)) = buckets
            .iter_mut()
            .find(|(bucket_start, _)| *bucket_start == record_slot)
        {
            apply_bucket(bucket, record);
        }
    }

    let requests = buckets
        .iter()
        .filter(|(bucket_start, _)| *bucket_start <= current_slot)
        .map(|(_, bucket)| bucket.requests)
        .sum::<u64>();
    let errors = buckets
        .iter()
        .filter(|(bucket_start, _)| *bucket_start <= current_slot)
        .map(|(_, bucket)| bucket.errors)
        .sum::<u64>();
    let successes = requests.saturating_sub(errors);
    let reliability = if requests > 0 {
        (successes as f64 / requests as f64 * 1000.0).round() / 10.0
    } else {
        0.0
    };
    DashboardHealthTimelineData {
        title: "请求健康时间线".to_string(),
        description: "请求可靠性".to_string(),
        reliability_display: if requests > 0 {
            format!("{reliability:.1}%")
        } else {
            "-".to_string()
        },
        points: buckets
            .into_iter()
            .map(|(bucket_start, bucket)| {
                let success_rate = if bucket.requests > 0 {
                    (bucket.requests.saturating_sub(bucket.errors) as f64 / bucket.requests as f64
                        * 1000.0)
                        .round()
                        / 10.0
                } else {
                    0.0
                };
                health_tone(bucket, success_rate, bucket_start > current_slot)
            })
            .collect(),
    }
}

pub(super) fn health_tone(bucket: UsageWindow, success_rate: f64, is_future: bool) -> char {
    let successes = bucket.requests.saturating_sub(bucket.errors);
    if is_future {
        '0'
    } else if bucket.requests == 0 {
        '1'
    } else if successes == 0 {
        '2'
    } else if success_rate < 90.0 {
        '3'
    } else if successes < HEALTH_TIMELINE_STABLE_SUCCESS_THRESHOLD {
        '4'
    } else {
        '5'
    }
}

pub(super) fn trend_summary(
    kind: DashboardTrendKind,
    points: &[DashboardTrendPointData],
) -> Vec<DashboardTrendSummaryData> {
    match kind {
        DashboardTrendKind::Usage => vec![
            trend_summary_item(
                kind,
                "输入",
                points.iter().map(|point| point.input_tokens_value).sum(),
                None,
            ),
            trend_summary_item(
                kind,
                "输出",
                points.iter().map(|point| point.output_tokens_value).sum(),
                None,
            ),
            trend_summary_item(
                kind,
                "缓存",
                points.iter().map(|point| point.cached_tokens_value).sum(),
                None,
            ),
        ],
        DashboardTrendKind::Latency => {
            let samples = points
                .iter()
                .filter(|point| point.latency_value > 0)
                .collect::<Vec<_>>();
            let avg = if samples.is_empty() {
                0
            } else {
                samples.iter().map(|point| point.latency_value).sum::<u64>() / samples.len() as u64
            };
            vec![
                trend_summary_item(kind, "平均", avg, None),
                trend_summary_item(
                    kind,
                    "最高",
                    samples
                        .iter()
                        .map(|point| point.max_latency_value)
                        .max()
                        .unwrap_or(0),
                    None,
                ),
                trend_summary_item(
                    kind,
                    "最低",
                    samples
                        .iter()
                        .filter_map(|point| {
                            (point.min_latency_value > 0).then_some(point.min_latency_value)
                        })
                        .min()
                        .unwrap_or(0),
                    None,
                ),
            ]
        }
        DashboardTrendKind::Errors => {
            let errors = points.iter().map(|point| point.errors_value).sum::<u64>();
            let requests = points.iter().map(|point| point.requests_value).sum::<u64>();
            let success_rate = if requests > 0 {
                Some(((requests - errors) as f64 / requests as f64 * 1000.0).round() / 10.0)
            } else {
                None
            };
            vec![
                trend_summary_item(kind, "错误数", errors, None),
                trend_summary_item(kind, "成功率", 0, success_rate),
                trend_summary_item(kind, "总请求", requests, None),
            ]
        }
    }
}

pub(super) fn trend_summary_item(
    kind: DashboardTrendKind,
    label: &str,
    value: u64,
    ratio: Option<f64>,
) -> DashboardTrendSummaryData {
    DashboardTrendSummaryData {
        label: label.to_string(),
        value: trend_summary_value_display(kind, value),
        ratio: ratio.map(format_percent),
    }
}

pub(super) fn trend_summary_value_display(kind: DashboardTrendKind, value: u64) -> String {
    match kind {
        DashboardTrendKind::Usage => format_tokens(value),
        DashboardTrendKind::Latency => format_optional_duration_ms(Some(value)),
        DashboardTrendKind::Errors => format_compact_number(value),
    }
}

pub(super) fn usage_window(
    records: &[AccountUsageTimeBucket],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> UsageWindow {
    let mut window = UsageWindow::default();
    for record in records {
        if record.bucket_start >= start && record.bucket_start < end {
            apply_bucket(&mut window, record);
        }
    }
    window
}

pub(super) fn cost_window(
    records: &[AccountUsageTimeBucket],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Option<f64> {
    let mut total = 0.0;
    let mut has_usage = false;
    for record in records {
        if record.bucket_start >= start && record.bucket_start < end {
            if let Some(cost) = record.cost_usd {
                total += cost;
                has_usage = true;
            }
        }
    }
    has_usage.then_some(total)
}

pub(super) fn apply_bucket(window: &mut UsageWindow, record: &AccountUsageTimeBucket) {
    window.requests += nonnegative_i64_to_u64(record.request_count);
    window.input_tokens += nonnegative_i64_to_u64(record.input_tokens);
    window.output_tokens += nonnegative_i64_to_u64(record.output_tokens);
    window.cached_tokens += nonnegative_i64_to_u64(record.cached_tokens);
    window.errors += nonnegative_i64_to_u64(record.error_count);
    window.first_token_latency_sum += nonnegative_i64_to_u64(record.first_token_latency_sum);
    window.first_token_latency_count += nonnegative_i64_to_u64(record.first_token_latency_count);
    let bucket_first_token_latency = first_token_bucket_latency(record);
    if let Some(latency) = bucket_first_token_latency {
        window.max_first_token_bucket_latency = window.max_first_token_bucket_latency.max(latency);
        window.min_first_token_bucket_latency = if window.min_first_token_bucket_latency == 0 {
            latency
        } else {
            window.min_first_token_bucket_latency.min(latency)
        };
    }
}

pub(super) fn first_token_bucket_latency(record: &AccountUsageTimeBucket) -> Option<u64> {
    let sum = nonnegative_i64_to_u64(record.first_token_latency_sum);
    let count = nonnegative_i64_to_u64(record.first_token_latency_count);
    sum.checked_div(count).filter(|latency| *latency > 0)
}

pub(super) fn account_pool_summary(
    accounts: &[Account],
    refreshing_account_ids: &HashSet<String>,
) -> AccountPoolDiagnostics {
    let mut summary = AccountPoolDiagnostics {
        total: accounts.len(),
        ..AccountPoolDiagnostics::default()
    };
    for account in accounts {
        match account.status {
            status
                if token_refresh_status_eligible(status)
                    && refreshing_account_ids.contains(&account.id) =>
            {
                summary.refreshing += 1;
            }
            AccountStatus::Active => summary.active += 1,
            AccountStatus::Expired => summary.expired += 1,
            AccountStatus::QuotaExhausted => summary.quota_exhausted += 1,
            AccountStatus::Disabled => summary.disabled += 1,
            AccountStatus::Banned => summary.banned += 1,
        }
    }
    summary
}

pub(super) fn account_usage_data(
    accounts: &[Account],
    usage_records: &[UsageRecordAccountUsage],
    quota_used_by_account: &HashMap<String, f64>,
    now: DateTime<Utc>,
) -> Vec<DashboardAccountUsageData> {
    let account_by_id = accounts
        .iter()
        .map(|account| (account.id.as_str(), account))
        .collect::<HashMap<_, _>>();
    usage_records
        .iter()
        .map(|usage| {
            let account = account_by_id.get(usage.account_id.as_str()).copied();
            let quota_used_percent = quota_used_by_account
                .get(&usage.account_id)
                .copied()
                .or_else(|| {
                    matches!(
                        account.map(|account| account.status),
                        Some(AccountStatus::QuotaExhausted)
                    )
                    .then_some(100.0)
                });
            DashboardAccountUsageData {
                id: usage.account_id.clone(),
                email: account
                    .and_then(|account| account.email.as_ref())
                    .cloned()
                    .unwrap_or_else(|| usage.account_id.clone()),
                plan_type: account
                    .and_then(|account| account.plan_type.as_ref())
                    .cloned(),
                tokens: format_tokens(usage.total_tokens),
                quota_used_percent,
                last_used: china_relative_time(Some(usage.last_used_at), now),
            }
        })
        .collect()
}

pub(super) async fn account_quota_used_percent_by_id(
    state: &AppState,
    usage_records: &[UsageRecordAccountUsage],
) -> HashMap<String, f64> {
    let mut quota_used_by_account = HashMap::with_capacity(usage_records.len());
    for usage in usage_records {
        let Ok(Some(quota_json)) = state
            .services
            .accounts
            .get_quota_json(&usage.account_id)
            .await
        else {
            continue;
        };
        if let Some(used_percent) = quota_used_percent(&quota_json) {
            quota_used_by_account.insert(usage.account_id.clone(), used_percent);
        }
    }
    quota_used_by_account
}

pub(super) fn quota_used_percent(quota_json: &str) -> Option<f64> {
    let quota = serde_json::from_str::<Value>(quota_json).ok()?;
    let mut selected = None;
    if let Some(monthly_limit) = quota.get("monthly_limit") {
        select_quota_used_percent(
            &mut selected,
            quota_window_priority(monthly_limit, QuotaWindowPriority::Monthly),
            monthly_limit.get("used_percent").and_then(percent_value),
        );
    }
    if let Some(snapshots) = quota.get("snapshots").and_then(Value::as_array) {
        for snapshot in snapshots {
            for role in ["primary", "secondary"] {
                if let Some(window) = snapshot.get(role) {
                    select_quota_used_percent(
                        &mut selected,
                        quota_window_priority(window, QuotaWindowPriority::Other),
                        window.get("used_percent").and_then(percent_value),
                    );
                }
            }
        }
    }
    selected.map(|(_, used_percent)| used_percent)
}

pub(super) fn select_quota_used_percent(
    selected: &mut Option<(QuotaWindowPriority, f64)>,
    priority: QuotaWindowPriority,
    used_percent: Option<f64>,
) {
    let Some(used_percent) = used_percent else {
        return;
    };
    match selected {
        Some((selected_priority, selected_percent))
            if priority > *selected_priority
                || (priority == *selected_priority && used_percent <= *selected_percent) => {}
        _ => *selected = Some((priority, used_percent)),
    }
}

pub(super) fn quota_window_priority(
    window: &Value,
    fallback: QuotaWindowPriority,
) -> QuotaWindowPriority {
    let window_seconds = window
        .get("window_minutes")
        .and_then(Value::as_u64)
        .and_then(|minutes| minutes.checked_mul(60));
    match window_seconds {
        Some(seconds) if quota_window_matches(seconds, FIVE_HOUR_WINDOW_SECONDS) => {
            QuotaWindowPriority::FiveHour
        }
        Some(seconds) if quota_window_matches(seconds, WEEK_WINDOW_SECONDS) => {
            QuotaWindowPriority::Weekly
        }
        Some(seconds) if quota_window_matches(seconds, MONTH_WINDOW_SECONDS) => {
            QuotaWindowPriority::Monthly
        }
        _ => fallback,
    }
}

pub(super) fn quota_window_matches(actual: u64, expected: u64) -> bool {
    actual > 0 && actual.abs_diff(expected) <= expected / 20
}

pub(super) fn percent_value(value: &Value) -> Option<f64> {
    let percent = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))?;
    percent.is_finite().then_some(percent.clamp(0.0, 100.0))
}

pub(super) fn dashboard_usage_record_items(
    items: Vec<UsageRecord>,
    account_emails: &HashMap<String, String>,
) -> Vec<DashboardUsageRecordData> {
    items
        .into_iter()
        .map(|record| dashboard_usage_record_data(record, account_emails))
        .collect()
}

pub(super) fn dashboard_usage_record_data(
    record: UsageRecord,
    account_emails: &HashMap<String, String>,
) -> DashboardUsageRecordData {
    let account_email = account_emails
        .get(&record.account_id)
        .cloned()
        .or_else(|| metadata_string(&record.metadata, &["accountEmail", "account_email"]));
    let (requested_model, upstream_model) = usage_record_models(&record);
    let client_ip = metadata_string(&record.metadata, &["clientIp", "ipAddress", "ip_address"]);
    let user_agent = metadata_string(&record.metadata, &["userAgent", "user_agent"]);
    let reasoning_effort =
        metadata_string(&record.metadata, &["reasoningEffort", "reasoning_effort"]);
    let token_details = usage_token_details(&record);
    let cost_details = usage_cost_details(&record, upstream_model.as_deref(), &token_details);
    let first_token_latency_ms = record.first_token_ms;

    DashboardUsageRecordData {
        id: record.id,
        route: record.route,
        model: record.model,
        status_code: record.status_code,
        transport: record.transport,
        created_at_display: china_datetime(&record.created_at),
        account_email,
        requested_model,
        upstream_model,
        client_ip,
        user_agent,
        reasoning_effort,
        token_details,
        cost_details,
        first_token_latency_ms_display: format_duration_ms(first_token_latency_ms),
        latency_ms_display: format_duration_ms(record.latency_ms),
    }
}

pub(super) fn service_status_data(state: &AppState) -> Vec<DashboardServiceStatusData> {
    let current_fingerprint = state.services.fingerprint.snapshot();
    let fingerprint = fingerprint_diagnostics(&current_fingerprint);
    vec![
        DashboardServiceStatusData {
            label: "客户端版本".to_string(),
            value: empty_dash(fingerprint.app_version),
            detail: if fingerprint.build_number.trim().is_empty() {
                "-".to_string()
            } else {
                format!("Build {}", fingerprint.build_number)
            },
            tone: "info".to_string(),
        },
        DashboardServiceStatusData {
            label: "平台架构".to_string(),
            value: empty_dash(fingerprint.platform),
            detail: empty_dash(fingerprint.arch),
            tone: "info".to_string(),
        },
        DashboardServiceStatusData {
            label: "Chromium".to_string(),
            value: if fingerprint.chromium_version.trim().is_empty() {
                "-".to_string()
            } else {
                format!("v{}", fingerprint.chromium_version)
            },
            detail: empty_dash(fingerprint.originator),
            tone: "normal".to_string(),
        },
        DashboardServiceStatusData {
            label: "更新时间".to_string(),
            value: format_fingerprint_updated_at(fingerprint.updated_at),
            detail: String::new(),
            tone: "normal".to_string(),
        },
        DashboardServiceStatusData {
            label: "User Agent".to_string(),
            value: fingerprint.user_agent,
            detail: String::new(),
            tone: "normal".to_string(),
        },
    ]
}

pub(super) fn empty_dash(value: String) -> String {
    if value.trim().is_empty() {
        "-".to_string()
    } else {
        value
    }
}

pub(super) fn format_fingerprint_updated_at(value: Option<String>) -> String {
    let Some(value) = value else {
        return "-".to_string();
    };
    let value = value.trim();
    if value.is_empty() {
        return "-".to_string();
    }
    china_datetime_rfc3339_str(value)
}
