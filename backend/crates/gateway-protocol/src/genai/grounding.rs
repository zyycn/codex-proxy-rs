//! 将一次完整搜索的 Google 来源与正文区间投影到同一套 Responses 事件

use crate::anthropic::TranslationError;
use serde_json::{Value, json};

pub fn grounded_search_events(
    native: &Value,
    mut events: Vec<Value>,
    include_sources: bool,
    event_budget: usize,
) -> Result<Vec<Value>, TranslationError> {
    let candidate = &native["candidates"][0];
    let metadata = &candidate["groundingMetadata"];
    let queries: Vec<_> = array(&metadata["webSearchQueries"])
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let chunks = array(&metadata["groundingChunks"]);
    if queries.is_empty() && !chunks.iter().any(|c| web_source(c).is_some()) {
        if candidate["finishReason"] == "STOP" {
            return Err(TranslationError::InvalidEvent(
                "Required web search returned no grounding evidence.",
            ));
        }
        return Ok(events);
    }
    let Some(response) = events.last_mut().and_then(|e| e.get_mut("response")) else {
        return Err(TranslationError::InvalidEvent(
            "Web search translation is missing its terminal response.",
        ));
    };
    let id = format!("ws_{}", response["id"].as_str().unwrap_or_default());
    let mut action = json!({"type":"search","queries":queries});
    if let Some(query) = queries.first() {
        action["query"] = json!(query)
    }
    if include_sources {
        action["sources"] = Value::Array(
            chunks
                .iter()
                .filter_map(web_source)
                .map(|(url, _)| json!({"type":"url","url":url}))
                .collect(),
        );
    }
    let call = json!({"type":"web_search_call","id":id,"status":"completed","action":action});
    let Some(output) = response["output"].as_array_mut() else {
        return Err(TranslationError::InvalidEvent(
            "Web search translation is missing its terminal response.",
        ));
    };
    let citations = annotations(candidate, output);
    for (item, annotations) in output.iter_mut().zip(&citations) {
        if !annotations.is_empty() {
            item["content"][0]["annotations"] = json!(annotations)
        }
    }
    output.insert(0, call.clone());
    // 单次搜索已经拿到完整正文，来源很多时仍通过 content_part.done 和终态交付全部引用
    let incremental_annotations =
        events.len() + 5 + citations.iter().map(Vec::len).sum::<usize>() <= event_budget;
    let mut result = Vec::with_capacity(events.len() + 5);
    for mut event in events {
        let kind = event["type"].as_str().unwrap_or_default();
        if kind == "response.in_progress" {
            result.push(event);
            let pending = json!({"type":"web_search_call","id":id,"status":"in_progress"});
            result
                .push(json!({"type":"response.output_item.added","output_index":0,"item":pending}));
            for stage in ["in_progress", "searching", "completed"] {
                result.push(json!({"type":format!("response.web_search_call.{stage}"),"output_index":0,"item_id":id}));
            }
            result.push(json!({"type":"response.output_item.done","output_index":0,"item":call}));
            continue;
        }
        if let Some(index) = index(&event["output_index"]) {
            if let Some(annotations) = citations.get(index).filter(|v| !v.is_empty()) {
                match kind {
                    "response.output_text.done" if incremental_annotations => {
                        for (i, annotation) in annotations.iter().enumerate() {
                            result.push(json!({"type":"response.output_text.annotation.added","item_id":event["item_id"],"output_index":index+1,"content_index":0,"annotation_index":i,"annotation":annotation}));
                        }
                    }
                    "response.content_part.done" => {
                        event["part"]["annotations"] = json!(annotations)
                    }
                    "response.output_item.done" => {
                        event["item"]["content"][0]["annotations"] = json!(annotations)
                    }
                    _ => {}
                }
            }
            event["output_index"] = json!(index + 1);
        }
        result.push(event);
    }
    for (i, event) in result.iter_mut().enumerate() {
        event["sequence_number"] = json!(i);
    }
    Ok(result)
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn index(value: &Value) -> Option<usize> {
    usize::try_from(value.as_u64()?).ok()
}

fn web_source(chunk: &Value) -> Option<(&str, &str)> {
    let web = &chunk["web"];
    let url = web["uri"].as_str()?;
    let uri = url.parse::<http::Uri>().ok()?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.host().is_none_or(str::is_empty) {
        return None;
    }
    Some((url, web["title"].as_str().unwrap_or(url)))
}

fn annotations(candidate: &Value, output: &[Value]) -> Vec<Vec<Value>> {
    let mut found = vec![Vec::new(); output.len()];
    let mut native_text = String::new();
    let mut parts = Vec::new();
    for part in array(&candidate["content"]["parts"]) {
        parts.push(
            part["text"]
                .as_str()
                .filter(|_| part["thought"] != true)
                .map(|text| {
                    let start = native_text.len();
                    native_text.push_str(text);
                    (start, text)
                }),
        );
    }
    // Google 的 offset 是单个 Part 的 UTF-8 字节；Responses 需要每个 output_text 内的字符索引
    let mut ranges = Vec::new();
    let mut end = 0;
    for (i, item) in output.iter().enumerate() {
        if let Some(text) = item.pointer("/content/0/text").and_then(Value::as_str) {
            if !native_text[end..].starts_with(text) {
                return found;
            }
            ranges.push((end, end + text.len(), i));
            end += text.len();
        }
    }
    if end != native_text.len() {
        return found;
    }
    let metadata = &candidate["groundingMetadata"];
    for support in array(&metadata["groundingSupports"]) {
        let segment = &support["segment"];
        let proto_index = |field| segment.get(field).map_or(Some(0), index);
        let Some(part) = proto_index("partIndex") else {
            continue;
        };
        let Some(&(offset, text)) = parts.get(part).and_then(Option::as_ref) else {
            continue;
        };
        let Some(start) = proto_index("startIndex") else {
            continue;
        };
        let Some(end) = index(&segment["endIndex"]) else {
            continue;
        };
        let Some(span) = text.get(start..end).filter(|s| !s.is_empty()) else {
            continue;
        };
        if segment["text"].as_str().is_some_and(|s| s != span) {
            continue;
        }
        let Some(&(begin, _, item)) = ranges
            .iter()
            .find(|(begin, limit, _)| offset + start >= *begin && offset + end <= *limit)
        else {
            continue;
        };
        let first = native_text[begin..offset + start].chars().count();
        let last = first + span.chars().count();
        for source in array(&support["groundingChunkIndices"]) {
            let Some((url, title)) = index(source)
                .and_then(|i| array(&metadata["groundingChunks"]).get(i))
                .and_then(web_source)
            else {
                continue;
            };
            found[item].push(json!({"type":"url_citation","url":url,"title":title,"start_index":first,"end_index":last}));
        }
    }
    found
}
