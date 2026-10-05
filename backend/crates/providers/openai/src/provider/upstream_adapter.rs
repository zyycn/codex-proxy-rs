//! 插件只替换业务 wire；账号租约、凭据解释与费用来源仍属于 OpenAI

use super::*;
use crate::transport::usage::{OpenAiBillingUsage, openai_billing_breakdown_with_override};
use gateway_core::{
    account::{CredentialRevision, OutboundProxy, ProviderAccountId},
    engine::upstream_adapter::{
        UpstreamAccountConnection, UpstreamAdapter, UpstreamAdapterInvocation,
    },
    metering::{CalculatedCost, Usage},
};

struct SelectedConnection {
    provider: Arc<CodexProvider>,
    lease: Arc<CodexCredentialLease>,
    context: AttemptContext,
    model: UpstreamModelId,
    requested_service_tier: Option<String>,
}

impl CodexProvider {
    // 与原生 terminal 共用一次选号结果，不能为适配器重新获取账号或租约
    #[expect(clippy::too_many_arguments)]
    pub(super) fn execute_upstream_adapter(
        self: Arc<Self>,
        operation: Operation,
        headers: Vec<MiddlewareHeader>,
        model: UpstreamModelId,
        context: AttemptContext,
        lease: CodexCredentialLease,
        account_selection_wait_ms: u64,
        requirements: CapabilityRequirements,
        adapter: Arc<dyn UpstreamAdapter>,
    ) -> Result<ProviderStream, ProviderError> {
        if operation.capability_requirements() != requirements {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let requested_service_tier = match &operation {
            Operation::Generate(generate)
                if generate.protocol_payload().protocol() == PROVIDER_NAME =>
            {
                let request =
                    CodexResponsesRequest::from_body(generate.protocol_payload().body().clone());
                normalize_service_tier(request.service_tier())
            }
            _ => None,
        };
        let lease = Arc::new(lease);
        let metadata = ProviderCallMetadata::new(
            ProviderKind::new(PROVIDER_NAME).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
            model.clone(),
            lease.account_id().clone(),
            UpstreamTransport::new(adapter.transport()).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
        )
        .with_selection_observation(ProviderSelectionObservation::new(
            account_selection_wait_ms,
            lease.capacity_snapshot(),
        ));
        let account = Arc::new(SelectedConnection {
            provider: Arc::clone(&self),
            lease: Arc::clone(&lease),
            context: context.clone(),
            model,
            requested_service_tier,
        });
        let events = adapter.execute(UpstreamAdapterInvocation {
            operation,
            headers,
            context,
            metadata: metadata.clone(),
            account,
        });
        let mutation = lease.allows_account_state_mutation();
        let stream = ProviderStream::new(metadata, events, lease);
        Ok(if mutation {
            stream.with_filtered_account_feedback(
                Arc::clone(&self.account_feedback),
                openai_failure_affects_account_score,
            )
        } else {
            stream
        })
    }
}

impl UpstreamAccountConnection for SelectedConnection {
    fn account_id(&self) -> &ProviderAccountId {
        self.lease.account_id()
    }
    fn credential_revision(&self) -> CredentialRevision {
        self.lease.account().revision()
    }
    fn authentication_kind(&self) -> &str {
        self.lease.account().authentication_kind()
    }
    fn outbound_proxy(&self) -> Option<&OutboundProxy> {
        self.lease.account().outbound_proxy()
    }

    fn authorization(&self) -> Result<Vec<MiddlewareHeader>, ProviderError> {
        let authorization = self
            .lease
            .authentication()
            .authorization_header()
            .map_err(|_| {
                provider_error(ProviderErrorKind::Unauthorized, UpstreamSendState::NotSent)
            })?;
        let mut headers = vec![MiddlewareHeader::new(
            "authorization",
            bytes::Bytes::copy_from_slice(authorization.expose_secret().as_bytes()),
        )];
        if let Some(account_id) = self.lease.account().upstream_account_id() {
            headers.push(MiddlewareHeader::new(
                "chatgpt-account-id",
                bytes::Bytes::copy_from_slice(account_id.as_bytes()),
            ));
        }
        // 业务目标可能与原生 Responses 不同，不能把原域名的 Cookie 带到新目标
        Ok(headers)
    }

    fn calculate_cost(&self, _: Option<&str>, usage: &Usage) -> Option<CalculatedCost> {
        // 与原生路径一样按请求策略计价；响应回显只作观测，不能切换模型或档位
        let usage = gateway_protocol::openai::events::TokenUsage {
            input_tokens: usage.input_tokens?,
            output_tokens: usage.output_tokens?,
            cached_tokens: usage.cached_tokens.unwrap_or(0),
            cache_write_tokens: usage.cache_write_tokens.unwrap_or(0),
            image_input_tokens: usage.image_input_tokens.unwrap_or(0),
            image_output_tokens: usage.image_output_tokens.unwrap_or(0),
            ..Default::default()
        };
        openai_billing_breakdown_with_override(
            self.model.as_str(),
            OpenAiBillingUsage::from(usage),
            self.requested_service_tier.as_deref(),
            self.context
                .pricing()
                .get(PROVIDER_NAME)
                .and_then(|models| models.get(self.model.as_str())),
        )
        .map(|cost| cost.calculated_cost())
    }

    fn record_failure(&self, error: ProviderError) -> BoxFuture<'_, ProviderError> {
        Box::pin(async move {
            if !self.lease.allows_account_state_mutation()
                || error.send_state() == UpstreamSendState::NotSent
            {
                return error;
            }
            let failure = match error.kind() {
                ProviderErrorKind::Unauthorized => Some(CodexAccountFailure::CredentialExpired),
                ProviderErrorKind::RateLimited => Some(CodexAccountFailure::RateLimited {
                    retry_after: error.retry_after(),
                }),
                ProviderErrorKind::QuotaExhausted => Some(CodexAccountFailure::QuotaExhausted),
                _ => None,
            };
            if let Some(failure) = failure {
                let _ = self
                    .provider
                    .selector
                    .record_failure(self.lease.account(), failure, None)
                    .await;
                if error.kind() == ProviderErrorKind::QuotaExhausted {
                    schedule_authoritative_quota_refresh_after_failure(
                        &self.provider.quota,
                        self.lease.account(),
                    );
                }
            }
            error
        })
    }
}
