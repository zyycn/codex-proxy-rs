use serde_json::Value;

pub(crate) fn retry_after_seconds_from_body(body: &str) -> Option<u64> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let error = value
        .pointer("/response/error")
        .or_else(|| value.get("error"))
        .unwrap_or(&value);
    if let Some(seconds) = error
        .get("resets_in_seconds")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds > 0)
    {
        return Some(seconds);
    }
    retry_after_seconds_from_resets_at(error)
        .or_else(|| retry_after_seconds_from_rate_limit_message(error))
}

fn retry_after_seconds_from_resets_at(error: &Value) -> Option<u64> {
    let resets_at = error.get("resets_at").and_then(Value::as_u64)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    (resets_at > now).then_some(resets_at - now)
}

fn retry_after_seconds_from_rate_limit_message(error: &Value) -> Option<u64> {
    let code = error
        .get("code")
        .or_else(|| error.get("type"))
        .and_then(Value::as_str)?;
    if code != "rate_limit_exceeded" {
        return None;
    }
    let message = error.get("message").and_then(Value::as_str)?;
    parse_try_again_delay_seconds(message)
}

fn parse_try_again_delay_seconds(message: &str) -> Option<u64> {
    let lower = message.to_ascii_lowercase();
    let marker = "try again in";
    let offset = lower.find(marker)? + marker.len();
    let remainder = message.get(offset..)?.trim_start();
    let number_end = number_prefix_len(remainder)?;
    let value = remainder.get(..number_end)?.parse::<f64>().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let unit_text = remainder
        .get(number_end..)?
        .trim_start()
        .to_ascii_lowercase();
    let unit = unit_token(&unit_text)?;
    match unit {
        "ms" => positive_seconds_ceil(value / 1000.0),
        "s" | "second" | "seconds" => positive_seconds_ceil(value),
        _ => None,
    }
}

fn number_prefix_len(input: &str) -> Option<usize> {
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut end = 0;
    for (index, ch) in input.char_indices() {
        if ch.is_ascii_digit() {
            seen_digit = true;
            end = index + ch.len_utf8();
        } else if ch == '.' && !seen_dot {
            seen_dot = true;
            end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    seen_digit.then_some(end)
}

fn unit_token(input: &str) -> Option<&str> {
    let end = input
        .char_indices()
        .find_map(|(index, ch)| (!ch.is_ascii_alphabetic()).then_some(index))
        .unwrap_or(input.len());
    (end > 0).then_some(&input[..end])
}

fn positive_seconds_ceil(seconds: f64) -> Option<u64> {
    if !seconds.is_finite() || seconds <= 0.0 || seconds > u64::MAX as f64 {
        return None;
    }
    Some(seconds.ceil() as u64)
}

#[cfg(test)]
mod tests {
    use super::retry_after_seconds_from_body;
    use serde_json::json;

    #[test]
    fn retry_after_seconds_from_body_should_parse_rate_limit_message_seconds() {
        let body = json!({
            "response": {
                "error": {
                    "code": "rate_limit_exceeded",
                    "message": "Rate limit reached. Please try again in 11.054s."
                }
            }
        })
        .to_string();

        assert_eq!(retry_after_seconds_from_body(&body), Some(12));
    }

    #[test]
    fn retry_after_seconds_from_body_should_parse_rate_limit_message_milliseconds() {
        let body = json!({
            "error": {
                "code": "rate_limit_exceeded",
                "message": "Rate limit reached. Please try again in 28ms."
            }
        })
        .to_string();

        assert_eq!(retry_after_seconds_from_body(&body), Some(1));
    }

    #[test]
    fn retry_after_seconds_from_body_should_ignore_retry_message_for_other_codes() {
        let body = json!({
            "response": {
                "error": {
                    "code": "upstream_transient_error",
                    "message": "Try again in 35 seconds."
                }
            }
        })
        .to_string();

        assert_eq!(retry_after_seconds_from_body(&body), None);
    }
}
