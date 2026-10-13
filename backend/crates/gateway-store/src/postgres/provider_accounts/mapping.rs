//! Admin 账号列表、用量与成本的领域映射

use crate::postgres::observability::{
    admin_account_model_usage, admin_cost_coverage as admin_account_cost_coverage,
};

use super::*;

pub(crate) fn admin_account_record(
    summary: ProviderAccountSummary,
) -> AdminStoreResult<AccountRecord> {
    Ok(AccountRecord {
        outbound_proxy: summary.outbound_proxy,
        id: summary.id,
        provider_kind: ProviderKind::new(summary.provider_kind).map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "persisted Provider kind is invalid",
            )
        })?,
        groups: Vec::new(),
        name: summary.name,
        notes: summary.notes,
        email: summary.email,
        upstream_user_id: summary.upstream_user_id,
        upstream_account_id: summary.upstream_account_id,
        plan_type: summary.plan_type,
        authentication_kind: summary.authentication_kind,
        credential_revision: admin_revision(summary.credential_revision)?,
        has_refresh_token: summary.has_refresh_token,
        access_token_expires_at: summary.access_token_expires_at,
        next_refresh_at: summary.next_refresh_at,
        enabled: summary.enabled,
        concurrency_limit: summary.concurrency_limit,
        weight: summary.weight,
        model_access: summary.model_access,
        credential_state: summary.credential_state,
        credential_observed_at: summary.credential_observed_at,
        quota: summary.quota,
        last_error_reason: summary.last_error_reason,
        last_error_message: summary.last_error_message,
        created_at: summary.created_at,
        updated_at: summary.updated_at,
    })
}

pub(crate) fn prepared_account(
    credential: PreparedCredentialCreate,
) -> StoreResult<NewProviderAccount> {
    Ok(NewProviderAccount {
        outbound_proxy: credential.outbound_proxy,
        id: credential.account_id.as_str().to_owned(),
        provider_kind: credential.provider_kind.as_str().to_owned(),
        name: credential.name,
        email: credential.email,
        upstream_user_id: credential.upstream_user_id,
        upstream_account_id: credential.upstream_account_id,
        plan_type: credential.plan_type,
        authentication_kind: credential.authentication_kind,
        provider_credentials_json: provider_document_json(credential.provider_material)?,
        has_refresh_token: credential.has_refresh_token,
        access_token_expires_at: credential.access_token_expires_at,
        next_refresh_at: credential.next_refresh_at,
        enabled: credential.enabled,
        concurrency_limit: None,
        weight: AccountWeight::DEFAULT,
        model_access: credential.model_access,
        credential_state: credential.credential_state,
        credential_observed_at: credential.credential_observed_at,
    })
}

pub(crate) fn provider_document_json(document: ProviderDocument) -> StoreResult<JsonObject> {
    JsonObject::try_from_value(
        ENTITY,
        serde_json::Value::Object(document.into_provider_data().into_inner()),
        CREDENTIALS_MAX_BYTES,
    )
}

pub(crate) fn admin_account_usage(usage: ProviderAccountUsageObservation) -> AccountUsage {
    AccountUsage {
        account_id: usage.account_id,
        request_count: usage.request_count,
        success_count: usage.success_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_write_input_tokens: usage.cache_write_input_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        image_input_tokens: usage.image_input_tokens,
        image_output_tokens: usage.image_output_tokens,
        image_request_count: usage.image_request_count,
        image_request_failed_count: usage.image_request_failed_count,
        total_tokens: usage.total_tokens,
        cost_coverage: admin_account_cost_coverage(usage.cost_coverage),
        costs: usage.costs,
        last_used_at: usage.last_used_at,
        request_buckets: usage.request_buckets,
        models: usage
            .models
            .into_iter()
            .map(admin_account_model_usage)
            .collect(),
    }
}
