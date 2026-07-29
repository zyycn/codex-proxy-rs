//! Grok Build 的账号隔离 reasoning replay。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine as _;
use gateway_core::event::ProviderEvent;
use serde_json::{Map, Value};

const REASONING_REPLAY_TTL: Duration = Duration::from_secs(60 * 60);
const REASONING_REPLAY_MAX_ENTRIES: usize = 1_024;
const REASONING_REPLAY_MAX_BYTES: usize = 8 * 1024 * 1024;
const REASONING_REPLAY_MAX_OUTPUT_ITEMS: usize = 4_096;
const MIN_REASONING_CIPHERTEXT_BYTES: usize = 50;
const MIN_REASONING_CIPHERTEXT_ENTROPY: f64 = 0.85;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GrokReasoningReplayKey {
    model: String,
    session_id: String,
    account_id: String,
}

#[derive(Clone)]
pub(crate) struct GrokReasoningReplay {
    state: Arc<Mutex<GrokReasoningReplayState>>,
}

#[derive(Default)]
struct GrokReasoningReplayState {
    entries: BTreeMap<GrokReasoningReplayKey, GrokReasoningReplayEntry>,
}

struct GrokReasoningReplayEntry {
    stored_at: Instant,
    items: Vec<Value>,
}

pub(crate) struct GrokReasoningReplayCapture {
    replay: GrokReasoningReplay,
    key: GrokReasoningReplayKey,
    output_items: BTreeMap<u32, Value>,
    captured_bytes: usize,
    truncated: bool,
    finished: bool,
}

impl Default for GrokReasoningReplay {
    fn default() -> Self {
        Self::new()
    }
}

impl GrokReasoningReplay {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(GrokReasoningReplayState::default())),
        }
    }

    pub(crate) fn key(
        &self,
        model: &str,
        session_id: &str,
        account_id: &str,
    ) -> Option<GrokReasoningReplayKey> {
        let model = model.trim();
        let session_id = session_id.trim();
        let account_id = account_id.trim();
        if model.is_empty() || session_id.is_empty() || account_id.is_empty() {
            return None;
        }
        Some(GrokReasoningReplayKey {
            model: model.to_owned(),
            session_id: session_id.to_owned(),
            account_id: account_id.to_owned(),
        })
    }

    pub(crate) fn apply(
        &self,
        key: &GrokReasoningReplayKey,
        input: &[Value],
    ) -> Option<Vec<Value>> {
        if input.is_empty() {
            return None;
        }
        let cached = self.read(key)?;
        let filtered = filter_replay_items_for_input(input, cached)?;
        Some(insert_replay_items(input, &filtered))
    }

    pub(crate) fn capture(&self, key: GrokReasoningReplayKey) -> GrokReasoningReplayCapture {
        GrokReasoningReplayCapture {
            replay: self.clone(),
            key,
            output_items: BTreeMap::new(),
            captured_bytes: 0,
            truncated: false,
            finished: false,
        }
    }

    pub(crate) fn clear(&self, key: &GrokReasoningReplayKey) {
        self.lock_state().entries.remove(key);
    }

    fn read(&self, key: &GrokReasoningReplayKey) -> Option<Vec<Value>> {
        let now = Instant::now();
        let mut state = self.lock_state();
        prune_expired(&mut state, now);
        state.entries.get(key).map(|entry| entry.items.clone())
    }

    fn store_output(&self, key: GrokReasoningReplayKey, output: Vec<Value>) {
        let normalized = match normalize_replay_items(output) {
            ReplayNormalization::Store(items) => items,
            ReplayNormalization::Delete => {
                self.clear(&key);
                return;
            }
            ReplayNormalization::Oversized => return,
        };
        let now = Instant::now();
        let mut state = self.lock_state();
        prune_expired(&mut state, now);
        if !state.entries.contains_key(&key)
            && state.entries.len() >= REASONING_REPLAY_MAX_ENTRIES
            && let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.stored_at)
                .map(|(key, _)| key.clone())
        {
            state.entries.remove(&oldest);
        }
        state.entries.insert(
            key,
            GrokReasoningReplayEntry {
                stored_at: now,
                items: normalized,
            },
        );
    }

    fn lock_state(&self) -> MutexGuard<'_, GrokReasoningReplayState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl GrokReasoningReplayCapture {
    pub(crate) fn observe(&mut self, events: &[ProviderEvent]) {
        if self.finished {
            return;
        }
        let mut completed = false;
        for event in events {
            let Some(wire) = event.wire_event() else {
                continue;
            };
            let payload = wire.data();
            let event_type = wire
                .event_type()
                .or_else(|| payload.get("type").and_then(Value::as_str));
            if event_type == Some("response.output_item.done")
                && let Some(item) = payload.get("item").cloned()
            {
                let index = payload
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_else(|| u32::try_from(self.output_items.len()).unwrap_or(u32::MAX));
                self.capture_item(index, item);
            }
            if event_type == Some("response.completed") {
                capture_terminal_output(payload, self);
                completed = true;
            }
        }
        if completed {
            self.finished = true;
            if self.truncated {
                self.output_items.clear();
                return;
            }
            let output = std::mem::take(&mut self.output_items)
                .into_values()
                .collect();
            self.replay.store_output(self.key.clone(), output);
        }
    }

    fn capture_item(&mut self, index: u32, item: Value) {
        if self.truncated || self.output_items.contains_key(&index) {
            return;
        }
        if self.output_items.len() >= REASONING_REPLAY_MAX_OUTPUT_ITEMS {
            self.truncate();
            return;
        }
        let Some(captured_bytes) = serde_json::to_vec(&item)
            .ok()
            .and_then(|encoded| self.captured_bytes.checked_add(encoded.len()))
            .filter(|captured_bytes| *captured_bytes <= REASONING_REPLAY_MAX_BYTES)
        else {
            self.truncate();
            return;
        };
        self.captured_bytes = captured_bytes;
        self.output_items.insert(index, item);
    }

    fn truncate(&mut self) {
        self.truncated = true;
        self.output_items.clear();
        self.captured_bytes = 0;
    }
}

fn capture_terminal_output(payload: &Value, capture: &mut GrokReasoningReplayCapture) {
    let Some(output) = payload
        .get("response")
        .and_then(Value::as_object)
        .and_then(|response| response.get("output"))
        .and_then(Value::as_array)
    else {
        return;
    };
    if output.len() > REASONING_REPLAY_MAX_OUTPUT_ITEMS {
        capture.truncate();
        return;
    }
    for (index, item) in output
        .iter()
        .take(REASONING_REPLAY_MAX_OUTPUT_ITEMS)
        .enumerate()
    {
        let Some(index) = u32::try_from(index).ok() else {
            break;
        };
        capture.capture_item(index, item.clone());
    }
}

fn prune_expired(state: &mut GrokReasoningReplayState, now: Instant) {
    state
        .entries
        .retain(|_, entry| now.saturating_duration_since(entry.stored_at) < REASONING_REPLAY_TTL);
}

enum ReplayNormalization {
    Store(Vec<Value>),
    Delete,
    Oversized,
}

fn normalize_replay_items(items: Vec<Value>) -> ReplayNormalization {
    let mut normalized = Vec::new();
    let mut has_anchor = false;
    for item in items {
        let Some(item) = normalize_replay_item(item) else {
            continue;
        };
        if matches!(
            item.get("type").and_then(Value::as_str),
            Some("reasoning" | "function_call" | "custom_tool_call")
        ) {
            has_anchor = true;
        }
        normalized.push(item);
    }
    if !has_anchor || normalized.is_empty() {
        return ReplayNormalization::Delete;
    }
    match serde_json::to_vec(&normalized) {
        Ok(encoded) if encoded.len() <= REASONING_REPLAY_MAX_BYTES => {
            ReplayNormalization::Store(normalized)
        }
        _ => ReplayNormalization::Oversized,
    }
}

fn normalize_replay_item(item: Value) -> Option<Value> {
    let object = item.as_object()?;
    match object.get("type").and_then(Value::as_str)?.trim() {
        "reasoning" => normalize_reasoning_item(object),
        "message" => normalize_assistant_message_item(object),
        "function_call" => normalize_function_call_item(object),
        "custom_tool_call" => normalize_custom_tool_call_item(object),
        _ => None,
    }
}

fn normalize_reasoning_item(item: &Map<String, Value>) -> Option<Value> {
    let encrypted = item.get("encrypted_content")?.as_str()?;
    if !valid_reasoning_ciphertext(encrypted) {
        return None;
    }
    Some(Value::Object(Map::from_iter([
        ("type".to_owned(), Value::String("reasoning".to_owned())),
        ("summary".to_owned(), Value::Array(Vec::new())),
        ("content".to_owned(), Value::Null),
        (
            "encrypted_content".to_owned(),
            Value::String(encrypted.to_owned()),
        ),
    ])))
}

fn normalize_assistant_message_item(item: &Map<String, Value>) -> Option<Value> {
    if !item
        .get("role")
        .and_then(Value::as_str)
        .is_some_and(|role| role.trim().eq_ignore_ascii_case("assistant"))
    {
        return None;
    }
    let parts = item
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(normalize_assistant_part)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    Some(Value::Object(Map::from_iter([
        ("type".to_owned(), Value::String("message".to_owned())),
        ("role".to_owned(), Value::String("assistant".to_owned())),
        ("content".to_owned(), Value::Array(parts)),
    ])))
}

fn normalize_assistant_part(part: &Value) -> Option<Value> {
    let part = part.as_object()?;
    match part.get("type").and_then(Value::as_str)?.trim() {
        "output_text" => Some(Value::Object(Map::from_iter([
            ("type".to_owned(), Value::String("output_text".to_owned())),
            (
                "text".to_owned(),
                Value::String(part.get("text")?.as_str()?.to_owned()),
            ),
        ]))),
        "refusal" => Some(Value::Object(Map::from_iter([
            ("type".to_owned(), Value::String("refusal".to_owned())),
            (
                "refusal".to_owned(),
                Value::String(part.get("refusal")?.as_str()?.to_owned()),
            ),
        ]))),
        _ => None,
    }
}

fn normalize_function_call_item(item: &Map<String, Value>) -> Option<Value> {
    let call_id = nonempty_string(item, "call_id")?;
    let name = nonempty_string(item, "name")?;
    let arguments = item.get("arguments")?.as_str()?.to_owned();
    Some(Value::Object(Map::from_iter([
        ("type".to_owned(), Value::String("function_call".to_owned())),
        ("call_id".to_owned(), Value::String(call_id)),
        ("name".to_owned(), Value::String(name)),
        ("arguments".to_owned(), Value::String(arguments)),
    ])))
}

fn normalize_custom_tool_call_item(item: &Map<String, Value>) -> Option<Value> {
    let call_id = nonempty_string(item, "call_id")?;
    let name = nonempty_string(item, "name")?;
    let input = item.get("input")?.clone();
    let status = item
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("completed")
        .to_owned();
    Some(Value::Object(Map::from_iter([
        (
            "type".to_owned(),
            Value::String("custom_tool_call".to_owned()),
        ),
        ("status".to_owned(), Value::String(status)),
        ("call_id".to_owned(), Value::String(call_id)),
        ("name".to_owned(), Value::String(name)),
        ("input".to_owned(), input),
    ])))
}

fn nonempty_string(item: &Map<String, Value>, field: &str) -> Option<String> {
    item.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn filter_replay_items_for_input(input: &[Value], cached: Vec<Value>) -> Option<Vec<Value>> {
    let last_assistant = input.iter().rev().find_map(assistant_message);
    let cached_assistant = cached.iter().find_map(assistant_message);
    let assistant_matches = match (last_assistant, cached_assistant) {
        (Some(current), Some(previous)) => {
            if !assistant_content_equal(current, previous) {
                return None;
            }
            true
        }
        _ => false,
    };

    let mut existing_calls = BTreeSet::new();
    let mut existing_outputs = BTreeMap::new();
    let mut existing_encrypted = BTreeSet::new();
    for item in input {
        let Some(item) = item.as_object() else {
            continue;
        };
        match item.get("type").and_then(Value::as_str).map(str::trim) {
            Some("reasoning") => {
                if let Some(encrypted) = item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    existing_encrypted.insert(encrypted.to_owned());
                }
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
                    for candidate in comparable_call_ids(call_id) {
                        existing_outputs.insert(candidate, call_id.trim().to_owned());
                    }
                }
            }
            Some(kind @ ("function_call" | "custom_tool_call")) => {
                if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
                    existing_calls.extend(replay_call_keys(kind, call_id));
                }
            }
            _ => {}
        }
    }

    let mut filtered = Vec::new();
    for mut item in cached {
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned();
        match kind.as_str() {
            "reasoning" => {
                if item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .is_some_and(|encrypted| existing_encrypted.contains(encrypted))
                {
                    continue;
                }
            }
            "message" if assistant_matches => continue,
            "message" => {}
            "function_call" | "custom_tool_call" => {
                let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                    continue;
                };
                let keys = replay_call_keys(&kind, call_id);
                if keys.is_empty() || keys.iter().any(|key| existing_calls.contains(key)) {
                    continue;
                }
                let output_call_id = comparable_call_ids(call_id)
                    .into_iter()
                    .find_map(|candidate| existing_outputs.get(&candidate).cloned());
                let Some(output_call_id) = output_call_id else {
                    continue;
                };
                existing_calls.extend(keys);
                if output_call_id != call_id
                    && let Some(item) = item.as_object_mut()
                {
                    item.insert("call_id".to_owned(), Value::String(output_call_id));
                }
            }
            _ => continue,
        }
        filtered.push(item);
    }
    (!filtered.is_empty()).then_some(filtered)
}

fn assistant_message(item: &Value) -> Option<&Map<String, Value>> {
    let item = item.as_object()?;
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let role = item.get("role")?.as_str()?.trim();
    ((item_type.is_empty() || item_type == "message") && role.eq_ignore_ascii_case("assistant"))
        .then_some(item)
}

fn assistant_content_equal(left: &Map<String, Value>, right: &Map<String, Value>) -> bool {
    let Some(left) = assistant_parts(left.get("content")) else {
        return false;
    };
    let Some(right) = assistant_parts(right.get("content")) else {
        return false;
    };
    left == right
}

fn assistant_parts(content: Option<&Value>) -> Option<Vec<(String, String)>> {
    match content? {
        Value::String(text) => Some(vec![("output_text".to_owned(), text.clone())]),
        Value::Array(parts) => {
            let mut result = Vec::new();
            for part in parts {
                let part = part.as_object()?;
                let kind = part.get("type")?.as_str()?.trim();
                let value = match kind {
                    "output_text" => part.get("text")?.as_str()?,
                    "refusal" => part.get("refusal")?.as_str()?,
                    _ => return None,
                };
                result.push((kind.to_owned(), value.to_owned()));
            }
            (!result.is_empty()).then_some(result)
        }
        _ => None,
    }
}

fn comparable_call_ids(call_id: &str) -> Vec<String> {
    let call_id = call_id.trim();
    if call_id.is_empty() {
        return Vec::new();
    }
    if let Some(upstream) = call_id
        .strip_prefix("toolu_")
        .filter(|value| !value.is_empty())
    {
        return vec![call_id.to_owned(), upstream.to_owned()];
    }
    vec![call_id.to_owned(), format!("toolu_{call_id}")]
}

fn replay_call_keys(item_type: &str, call_id: &str) -> Vec<String> {
    if !matches!(item_type, "function_call" | "custom_tool_call") {
        return Vec::new();
    }
    comparable_call_ids(call_id)
        .into_iter()
        .map(|call_id| format!("{item_type}\0{call_id}"))
        .collect()
}

fn insert_replay_items(input: &[Value], replay: &[Value]) -> Vec<Value> {
    let insert_at = replay_insert_index(input, replay);
    let mut result = Vec::new();
    for (index, item) in input.iter().enumerate() {
        if index == insert_at {
            result.extend(replay.iter().cloned());
        }
        result.push(item.clone());
    }
    if insert_at == input.len() {
        result.extend(replay.iter().cloned());
    }
    result
}

fn replay_insert_index(input: &[Value], replay: &[Value]) -> usize {
    let replay_call_ids = replay
        .iter()
        .filter_map(Value::as_object)
        .filter(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call" | "custom_tool_call")
            )
        })
        .filter_map(|item| item.get("call_id").and_then(Value::as_str))
        .flat_map(comparable_call_ids)
        .collect::<BTreeSet<_>>();
    if !replay_call_ids.is_empty() {
        for (index, item) in input.iter().enumerate() {
            let Some(item) = item.as_object() else {
                continue;
            };
            if !matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call_output" | "custom_tool_call_output")
            ) {
                continue;
            }
            let call_ids = item
                .get("call_id")
                .and_then(Value::as_str)
                .map(comparable_call_ids)
                .unwrap_or_default();
            if call_ids.is_empty()
                || call_ids
                    .iter()
                    .any(|call_id| replay_call_ids.contains(call_id))
            {
                return index;
            }
        }
    }
    if let Some(index) = input
        .iter()
        .rposition(|item| assistant_message(item).is_some())
    {
        return index;
    }
    input
        .iter()
        .position(should_insert_replay_before)
        .unwrap_or(input.len())
}

fn should_insert_replay_before(item: &Value) -> bool {
    let Some(item) = item.as_object() else {
        return true;
    };
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let role = item
        .get("role")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if role.is_empty() || (!item_type.is_empty() && item_type != "message") {
        return true;
    }
    !matches!(role.as_str(), "system" | "developer")
}

pub(crate) fn valid_reasoning_ciphertext(value: &str) -> bool {
    if value.is_empty()
        || value != value.trim()
        || value.len() > REASONING_REPLAY_MAX_BYTES
        || value.starts_with("gAAAA")
        || value.contains('=')
    {
        return false;
    }
    let Ok(decoded) = base64::engine::general_purpose::STANDARD_NO_PAD.decode(value) else {
        return false;
    };
    decoded.len() >= MIN_REASONING_CIPHERTEXT_BYTES
        && byte_entropy_ratio(&decoded) >= MIN_REASONING_CIPHERTEXT_ENTROPY
}

fn byte_entropy_ratio(value: &[u8]) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let mut counts = [0_u32; 256];
    for byte in value {
        counts[usize::from(*byte)] += 1;
    }
    let size = value.len() as f64;
    let entropy = counts
        .into_iter()
        .filter(|count| *count > 0)
        .map(|count| {
            let probability = f64::from(count) / size;
            -probability * probability.log2()
        })
        .sum::<f64>();
    let symbols = value.len().min(256);
    if symbols <= 1 {
        return 0.0;
    }
    entropy / (symbols as f64).log2()
}
