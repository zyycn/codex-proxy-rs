//! xAI 已选 OAuth 会话的受管连接；插件不读取令牌或重新选号

use super::*;
use gateway_core::{
    account::{CredentialRevision, OutboundProxy},
    engine::upstream_adapter::{
        UpstreamAccountConnection, UpstreamAdapter, UpstreamAdapterInvocation,
    },
    metering::{CalculatedCost, Usage},
};

struct SelectedConnection {
    provider: Arc<GrokBuildProvider>,
    session: Arc<SelectedGrokSession>,
    context: AttemptContext,
    model: UpstreamModelId,
}

impl GrokBuildProvider {
    pub(super) fn execute_upstream_adapter(
        self: Arc<Self>,
        operation: Operation,
        headers: Vec<MiddlewareHeader>,
        selected: SelectedGrokAttempt,
        adapter: Arc<dyn UpstreamAdapter>,
    ) -> Result<ProviderStream, ProviderError> {
        if operation.capability_requirements() != selected.frozen_requirements {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let session = Arc::new(selected.selected);
        let metadata = ProviderCallMetadata::new(
            ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| protocol_not_sent())?,
            selected.upstream_model.clone(),
            session.account_id().clone(),
            UpstreamTransport::new(adapter.transport()).map_err(|_| protocol_not_sent())?,
        )
        .with_selection_observation(ProviderSelectionObservation::new(
            selected.account_selection_wait_ms,
            session.capacity_snapshot(),
        ));
        let account = Arc::new(SelectedConnection {
            provider: Arc::clone(&self),
            session: Arc::clone(&session),
            context: selected.context.clone(),
            model: selected.upstream_model,
        });
        let events = adapter.execute(UpstreamAdapterInvocation {
            operation,
            headers,
            context: selected.context,
            metadata: metadata.clone(),
            account,
        });
        let mutation = session.allows_account_state_mutation();
        let stream = ProviderStream::new(metadata, events, session);
        Ok(if mutation {
            // 插件协议或 RPC 故障不能降低原生账号评分；只接纳已解析的上游拒绝事实
            stream.with_filtered_account_feedback(Arc::clone(&self.account_feedback), |error| {
                error.client_visible_upstream_error().is_some()
            })
        } else {
            stream
        })
    }
}

impl UpstreamAccountConnection for SelectedConnection {
    fn account_id(&self) -> &ProviderAccountId {
        self.session.account_id()
    }
    fn credential_revision(&self) -> CredentialRevision {
        self.session.credential_revision()
    }
    fn authentication_kind(&self) -> &str {
        crate::credential::XAI_AUTHENTICATION_KIND_OAUTH
    }
    fn outbound_proxy(&self) -> Option<&OutboundProxy> {
        self.session.binding().outbound_proxy()
    }

    fn authorization(&self) -> Result<Vec<MiddlewareHeader>, ProviderError> {
        Ok(vec![
            MiddlewareHeader::new(
                "authorization",
                bytes::Bytes::from(format!("Bearer {}", self.session.access_token().expose())),
            ),
            MiddlewareHeader::new(
                "x-grok-user-id",
                bytes::Bytes::copy_from_slice(self.session.user_id().expose().as_bytes()),
            ),
            MiddlewareHeader::new(
                "x-xai-token-auth",
                bytes::Bytes::from_static(b"xai-grok-cli"),
            ),
        ])
    }

    fn calculate_cost(&self, tier: Option<&str>, usage: &Usage) -> Option<CalculatedCost> {
        crate::transport::canonical::grok_billing_breakdown_with_override(
            self.model.as_str(),
            usage.input_tokens?,
            usage.output_tokens?,
            usage.cached_tokens.unwrap_or(0),
            usage.cache_write_tokens.unwrap_or(0),
            tier,
            self.context
                .pricing()
                .get(XAI_PROVIDER_NAME)
                .and_then(|models| models.get(self.model.as_str())),
        )
        .map(|cost| cost.calculated_cost())
    }

    fn record_failure(&self, mut error: ProviderError) -> BoxFuture<'_, ProviderError> {
        Box::pin(async move {
            if error.send_state() == UpstreamSendState::NotSent {
                return error;
            }
            if error.kind() == ProviderErrorKind::Unauthorized {
                error = error.with_credential_recovery();
            }
            let failure = stream_credential_failure(&error, &self.model);
            recover_or_record_failure(
                self.provider.selector.as_ref(),
                self.provider.credential_recovery.as_ref(),
                &self.session,
                error,
                failure,
                self.context.credential_recovery_attempted(),
            )
            .await
        })
    }
}
