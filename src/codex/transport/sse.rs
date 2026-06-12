use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SseError {
    #[error("invalid SSE retry value: {0}")]
    InvalidRetry(String),
}

#[derive(Debug, Default)]
struct EventBuilder {
    event: Option<String>,
    data: String,
    has_data: bool,
    id: Option<String>,
    retry: Option<u64>,
}

impl EventBuilder {
    fn push_data(&mut self, value: &str) {
        if self.has_data {
            self.data.push('\n');
        }
        self.data.push_str(value);
        self.has_data = true;
    }

    fn finish(&mut self) -> Option<SseEvent> {
        if !self.has_data {
            self.event = None;
            self.id = None;
            self.retry = None;
            return None;
        }
        self.has_data = false;
        Some(SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data),
            id: self.id.take(),
            retry: self.retry.take(),
        })
    }
}

pub fn parse_sse_events(input: &str) -> Result<Vec<SseEvent>, SseError> {
    let mut events = Vec::new();
    let mut builder = EventBuilder::default();

    for raw_line in input.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            if let Some(event) = builder.finish() {
                events.push(event);
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }

        let (field, value) = split_sse_field(line);
        match field {
            "event" => builder.event = Some(value.to_string()),
            "data" => builder.push_data(value),
            "id" if !value.contains('\0') => builder.id = Some(value.to_string()),
            "retry" => {
                builder.retry = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| SseError::InvalidRetry(value.to_string()))?,
                );
            }
            _ => {}
        }
    }

    if let Some(event) = builder.finish() {
        events.push(event);
    }
    Ok(events)
}

pub fn encode_sse_event(event: &str, data: &str) -> String {
    let mut frame = String::new();
    if !event.is_empty() {
        frame.push_str("event: ");
        frame.push_str(event);
        frame.push('\n');
    }
    for line in data.split('\n') {
        frame.push_str("data: ");
        frame.push_str(line);
        frame.push('\n');
    }
    frame.push('\n');
    frame
}

fn split_sse_field(line: &str) -> (&str, &str) {
    let Some((field, value)) = line.split_once(':') else {
        return (line, "");
    };
    (field, value.strip_prefix(' ').unwrap_or(value))
}
