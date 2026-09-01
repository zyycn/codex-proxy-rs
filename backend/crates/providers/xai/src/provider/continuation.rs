//! xAI continuation 与 reasoning replay 状态处理。

use super::*;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct XaiSessionState {
    pub(super) account_id: String,
    pub(super) session_id: Option<String>,
    pub(super) transcript: Vec<XaiReplayItem>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum XaiReplayItem {
    ClientInput(Value),
    SanitizedOutput(Value),
    AccountOutput { account_id: String, item: Value },
}

pub(super) struct GrokSessionCapture {
    pub(super) previous: Option<XaiSessionState>,
    pub(super) request_input: Vec<Value>,
    pub(super) account_id: String,
    pub(super) session_id: Option<String>,
    pub(super) output_items: BTreeMap<u32, Value>,
}

pub(super) fn decode_xai_session_state(
    request: &GenerateRequest,
) -> Result<Option<XaiSessionState>, ProviderError> {
    request
        .provider_session_state(XAI_PROVIDER_NAME)
        .map(|state| {
            let payload = Value::Object(state.payload().clone());
            if serde_json::to_vec(&payload)
                .map_err(|_| protocol_not_sent())?
                .len()
                > XAI_SESSION_STATE_MAX_BYTES
            {
                return Err(protocol_not_sent());
            }
            serde_json::from_value(payload).map_err(|_| protocol_not_sent())
        })
        .transpose()
}

pub(super) fn encode_xai_session_state(
    state: XaiSessionState,
) -> Result<Option<ProviderSessionState>, ProviderError> {
    let value = serde_json::to_value(state).map_err(|_| protocol_sent())?;
    if serde_json::to_vec(&value)
        .map_err(|_| protocol_sent())?
        .len()
        > XAI_SESSION_STATE_MAX_BYTES
    {
        return Ok(None);
    }
    let Value::Object(payload) = value else {
        return Err(protocol_sent());
    };
    ProviderSessionState::new(XAI_PROVIDER_NAME, payload)
        .map(Some)
        .map_err(|_| protocol_sent())
}

pub(super) fn continuation_account(
    context: &AttemptContext,
    previous_session: Option<&XaiSessionState>,
) -> Result<Option<gateway_core::account::ProviderAccountId>, ProviderError> {
    let Some(continuation) = context.continuation() else {
        return Ok(None);
    };
    match (context.continuation_attempt(), continuation) {
        (ContinuationAttempt::Native, ContinuationBinding::Pinned(pin)) => {
            if pin.provider().as_str() != XAI_PROVIDER_NAME {
                return Err(invalid_continuation());
            }
            Ok(Some(pin.account().clone()))
        }
        (ContinuationAttempt::Native, ContinuationBinding::External(_)) => {
            Err(invalid_continuation())
        }
        (ContinuationAttempt::ReplayOwner, _) => {
            let previous = previous_session.ok_or_else(invalid_continuation)?;
            gateway_core::account::ProviderAccountId::new(previous.account_id.clone())
                .map(Some)
                .map_err(|_| invalid_continuation())
        }
        (ContinuationAttempt::ReplayAny, _) => Ok(None),
        (ContinuationAttempt::None, _) => Err(invalid_continuation()),
    }
}

pub(super) fn apply_continuation(
    request: &mut GrokResponsesRequest,
    previous_session: Option<&XaiSessionState>,
    context: &AttemptContext,
    account: &gateway_core::account::ProviderAccountId,
    current_input: &[Value],
) -> Result<(), ProviderError> {
    let Some(continuation) = context.continuation() else {
        return Ok(());
    };
    match context.continuation_attempt() {
        ContinuationAttempt::Native => {
            let ContinuationBinding::Pinned(pin) = continuation else {
                return Err(invalid_continuation());
            };
            let provider = ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| protocol_not_sent())?;
            if !pin.matches(&provider, account) {
                return Err(invalid_continuation());
            }
            request.set_previous_response_id(Some(pin.upstream_response_id().as_str().to_owned()));
            Ok(())
        }
        ContinuationAttempt::ReplayOwner | ContinuationAttempt::ReplayAny => {
            let previous = previous_session.ok_or_else(invalid_continuation)?;
            if context.continuation_attempt() == ContinuationAttempt::ReplayOwner
                && previous.account_id != account.as_str()
            {
                return Err(invalid_continuation());
            }
            let mut input = replay_input_for_account(previous, account.as_str(), true);
            input.reserve(current_input.len());
            input.extend(current_input.iter().cloned());
            request.set_replay_input(input).map_err(map_request_error)?;
            request.set_previous_response_id(None);
            request.inherit_session(None);
            Ok(())
        }
        ContinuationAttempt::None => Err(invalid_continuation()),
    }
}

pub(super) fn replay_input_for_account(
    state: &XaiSessionState,
    account_id: &str,
    force_portable: bool,
) -> Vec<Value> {
    state
        .transcript
        .iter()
        .filter_map(|item| match item {
            XaiReplayItem::ClientInput(value) | XaiReplayItem::SanitizedOutput(value) => {
                Some(value.clone())
            }
            XaiReplayItem::AccountOutput {
                account_id: owner,
                item,
            } if owner == account_id && !force_portable => {
                portable_output_item(item.clone(), false)
            }
            XaiReplayItem::AccountOutput { item, .. } => portable_output_item(item.clone(), true),
        })
        .collect()
}

pub(super) fn project_transcript_to_account(transcript: &mut Vec<XaiReplayItem>, account_id: &str) {
    *transcript = transcript
        .drain(..)
        .filter_map(|item| match item {
            XaiReplayItem::AccountOutput {
                account_id: owner,
                item,
            } if owner != account_id => {
                portable_output_item(item, true).map(XaiReplayItem::SanitizedOutput)
            }
            item => Some(item),
        })
        .collect();
}

pub(super) fn portable_output_item(mut item: Value, strip_opaque: bool) -> Option<Value> {
    let Value::Object(object) = &mut item else {
        return None;
    };
    let is_reasoning = object.get("type").and_then(Value::as_str) == Some("reasoning");
    if !matches!(
        object.get("type").and_then(Value::as_str),
        Some("reasoning" | "message" | "function_call" | "custom_tool_call")
    ) {
        return None;
    }
    object.remove("id");
    object.remove("status");
    if is_reasoning {
        if strip_opaque
            || object
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_some_and(|value| !valid_reasoning_ciphertext(value))
        {
            object.remove("encrypted_content");
        }
        if object.get("encrypted_content").is_none() && !has_readable_reasoning(object) {
            return None;
        }
    }
    Some(item)
}

pub(super) fn has_readable_reasoning(item: &Map<String, Value>) -> bool {
    ["summary", "content"].into_iter().any(|field| {
        item.get(field)
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.trim().is_empty())
                })
            })
    })
}

pub(super) fn attach_xai_session_update(
    events: &mut [ProviderEvent],
    capture: &mut Option<GrokSessionCapture>,
) -> Result<(), ProviderError> {
    if capture.is_none() {
        return Ok(());
    }
    let mut terminal_index = None;
    for (index, event) in events.iter().enumerate() {
        if let Some(capture) = capture.as_mut() {
            capture_output_item(event, capture);
        }
        if event
            .canonical_facts()
            .iter()
            .any(|fact| matches!(fact, GatewayEvent::Completed(_)))
        {
            terminal_index = Some(index);
        }
    }
    let Some(terminal_index) = terminal_index else {
        return Ok(());
    };
    let Some(mut capture) = capture.take() else {
        return Ok(());
    };
    let mut transcript = capture
        .previous
        .take()
        .map(|state| state.transcript)
        .unwrap_or_default();
    project_transcript_to_account(&mut transcript, &capture.account_id);
    transcript.extend(
        capture
            .request_input
            .into_iter()
            .map(XaiReplayItem::ClientInput),
    );
    transcript.extend(capture.output_items.into_values().map(|item| {
        XaiReplayItem::AccountOutput {
            account_id: capture.account_id.clone(),
            item,
        }
    }));
    let state = XaiSessionState {
        account_id: capture.account_id,
        session_id: capture.session_id,
        transcript,
    };
    if let Some(update) = encode_xai_session_state(state)? {
        events[terminal_index].attach_session_update(update);
    }
    Ok(())
}

pub(super) fn capture_output_item(event: &ProviderEvent, capture: &mut GrokSessionCapture) {
    let Some(wire) = event.wire_event() else {
        return;
    };
    let event_type = wire
        .event_type()
        .or_else(|| wire.data().get("type").and_then(Value::as_str));
    if event_type == Some("response.output_item.done")
        && capture.output_items.len() < XAI_SESSION_OUTPUT_LIMIT
        && let Some(item) = wire
            .data()
            .get("item")
            .cloned()
            .and_then(|item| portable_output_item(item, false))
    {
        let index = wire
            .data()
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_else(|| u32::try_from(capture.output_items.len()).unwrap_or(u32::MAX));
        capture.output_items.insert(index, item);
    }
    if matches!(
        event_type,
        Some("response.completed" | "response.incomplete")
    ) && let Some(output) = wire
        .data()
        .get("response")
        .and_then(Value::as_object)
        .and_then(|response| response.get("output"))
        .and_then(Value::as_array)
        .filter(|output| !output.is_empty())
    {
        for (index, item) in output.iter().take(XAI_SESSION_OUTPUT_LIMIT).enumerate() {
            let Some(index) = u32::try_from(index).ok() else {
                break;
            };
            if capture.output_items.contains_key(&index) {
                continue;
            }
            if let Some(item) = portable_output_item(item.clone(), false) {
                capture.output_items.insert(index, item);
            }
        }
    }
}

pub(super) fn invalid_continuation() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidRequest,
        UpstreamSendState::NotSent,
    )
    .with_continuation_failure(ContinuationFailure::HistoryUnavailable)
    .with_continuation_recovery_disposition(ContinuationRecoveryDisposition::ClientReplayRequired)
}

pub(super) fn protocol_not_sent() -> ProviderError {
    provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
}

pub(super) fn protocol_sent() -> ProviderError {
    provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
}
