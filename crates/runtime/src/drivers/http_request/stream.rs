//! Decoding a streamed response: SSE blocks and JSON lines.

use super::*;

pub(super) fn stream_event(data: String, event: Option<String>, id: Option<String>) -> Value {
    object(vec![
        ("data", Value::string(data)),
        ("event", event.map_or(Value::Null, Value::string)),
        ("id", id.map_or(Value::Null, Value::string)),
    ])
}

pub(super) fn sse_block(block: &str) -> Option<Value> {
    let mut data: Option<Vec<&str>> = None;
    let mut event = None;
    let mut id = None;
    for line in block.split('\n') {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "data" => data.get_or_insert_with(Vec::new).push(value),
            "event" => event = Some(value.to_string()),
            "id" => id = Some(value.to_string()),
            _ => {}
        }
    }
    data.map(|lines| stream_event(lines.join("\n"), event, id))
}

pub(super) fn json_line(line: &str) -> Option<Value> {
    let line = line.trim_end_matches('\r');
    if line.is_empty() {
        None
    } else {
        Some(stream_event(line.to_string(), None, None))
    }
}

/// Incremental UTF-8 decoding and the shared stream framer, retaining incomplete code points between reads.
pub struct StreamDecoder {
    pub(super) framing: StreamFraming,
    pub(super) key: Option<String>,
    pub(super) bytes: Vec<u8>,
    pub(super) carry: String,
}

impl StreamDecoder {
    pub fn new(framing: StreamFraming, key: Option<&str>) -> Self {
        Self { framing, key: key.map(str::to_string), bytes: Vec::new(), carry: String::new() }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<Value> {
        self.bytes.extend_from_slice(bytes);
        let mut text = String::new();
        loop {
            match std::str::from_utf8(&self.bytes) {
                Ok(valid) => {
                    text.push_str(valid);
                    self.bytes.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(&String::from_utf8_lossy(&self.bytes[..valid]));
                    match error.error_len() {
                        Some(invalid) => {
                            text.push('\u{fffd}');
                            self.bytes.drain(..valid + invalid);
                        }
                        None => {
                            self.bytes.drain(..valid);
                            break;
                        }
                    }
                }
            }
        }
        frame_stream(self.framing, self.key.as_deref(), &text, &mut self.carry)
    }

    pub fn finish(&mut self) -> Vec<Value> {
        let tail = String::from_utf8_lossy(&std::mem::take(&mut self.bytes)).into_owned();
        let mut events = frame_stream(self.framing, self.key.as_deref(), &tail, &mut self.carry);
        events.extend(frame_stream_finish(self.framing, self.key.as_deref(), &mut self.carry));
        events
    }
}

/// Frames `chunk` appended to `carry`; whatever is not yet a whole frame stays in `carry`.
pub fn stream_events(framing: StreamFraming, chunk: &str, carry: &mut String) -> Vec<Value> {
    carry.push_str(chunk);
    match framing {
        StreamFraming::Sse => {
            let normalized = carry.replace("\r\n", "\n");
            let Some(end) = normalized.rfind("\n\n") else {
                *carry = normalized;
                return Vec::new();
            };
            let events = normalized[..end].split("\n\n").filter_map(sse_block).collect();
            *carry = normalized[end + 2..].to_string();
            events
        }
        StreamFraming::JsonLines => {
            let Some(end) = carry.rfind('\n') else {
                return Vec::new();
            };
            let events = carry[..end].split('\n').filter_map(json_line).collect();
            *carry = carry[end + 1..].to_string();
            events
        }
    }
}

/// Frames `chunk` appended to `carry` and stamps each event with the call's `key` (`null` when unkeyed): the one
/// place a stream event gets it, for the driver and for a host holding only an `HttpStream` ask.
pub fn frame_stream(framing: StreamFraming, key: Option<&str>, chunk: &str, carry: &mut String) -> Vec<Value> {
    let key = key.map(str::to_string);
    stream_events(framing, chunk, carry)
        .into_iter()
        .map(|e| with_key(e, &key))
        .collect()
}

/// The keyed events the unterminated tail in `carry` makes at end of body.
pub fn frame_stream_finish(framing: StreamFraming, key: Option<&str>, carry: &mut String) -> Vec<Value> {
    let key = key.map(str::to_string);
    stream_finish(framing, carry)
        .into_iter()
        .map(|e| with_key(e, &key))
        .collect()
}

/// The final unterminated frame at EOF.
pub fn stream_finish(framing: StreamFraming, carry: &mut String) -> Vec<Value> {
    let rest = std::mem::take(carry);
    match framing {
        StreamFraming::Sse => sse_block(&rest.replace("\r\n", "\n")).into_iter().collect(),
        StreamFraming::JsonLines => json_line(&rest).into_iter().collect(),
    }
}
