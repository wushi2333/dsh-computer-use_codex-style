//! Stdio JSONL helper protocol for the DSH Computer Use Linux helper.
//!
//! One JSON object per line in, one per line out:
//!
//! ```text
//! -> {"id":1,"method":"call","params":{"name":"list_apps","arguments":{}},"meta":{}}
//! <- {"id":1,"ok":true,"result":{"ok":true,"name":"list_apps","value":{...},"images":[]}}
//! ```
//!
//! The envelope is the one documented in `docs/recovered-protocol.md` and mirrored
//! by `helper-rs/src/protocol.rs`, so the plugin's JS sidecar talks to this helper
//! exactly as it talks to the Windows helper. The crate itself speaks MCP; this
//! module is the *transport* replacement, and `helper.rs` is the dispatcher that
//! reuses the crate's MCP tool handlers unchanged.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Methods this helper answers. Anything else is refused with
/// `unsupported method: <name>` (the official helper never says "unknown method").
pub const HELPER_METHODS: &[&str] = &[
    "health",
    "tools",
    "call",
    "interrupt",
    "shutdown",
    "prompt",
    "end_turn",
];

/// Official rdata 0x131c1c: unknown methods *and* unknown tools both report this.
pub const UNSUPPORTED_METHOD_PREFIX: &str = "unsupported method: ";

/// Shown when a request arrives after `interrupt`. Wording follows the official
/// helper so the model's behaviour is the same on every platform.
pub const INTERRUPTED_MESSAGE: &str = "Computer Use was stopped by the user. Stop your work, do not call further Computer Use tools in this turn, and send a final message noting that the user stopped Computer Use.";

/// Shown when a request arrives after `end_turn`.
pub const TURN_ENDED_MESSAGE: &str = "Computer Use is no longer available in this turn because the turn has ended. Do not call further Computer Use tools in this turn.";

/// Default request budget when `meta` carries none (official JS transport value).
pub const DEFAULT_BUDGET_MS: i64 = 15_000;
pub const BUDGET_HEADER: &str = "x-oai-cua-request-budget-ms";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Request {
    #[serde(default)]
    pub id: Value,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub meta: Value,
}

impl Request {
    pub fn method_name(&self) -> &str {
        self.method.as_deref().unwrap_or("")
    }

    pub fn params_object(&self) -> Map<String, Value> {
        match &self.params {
            Value::Object(map) => map.clone(),
            _ => Map::new(),
        }
    }

    pub fn meta_object(&self) -> Map<String, Value> {
        match &self.meta {
            Value::Object(map) => map.clone(),
            _ => Map::new(),
        }
    }

    pub fn budget_ms(&self) -> i64 {
        meta_i64(&self.meta, BUDGET_HEADER).unwrap_or(DEFAULT_BUDGET_MS)
    }
}

/// Parse one line into a request. A blank line is `Ok(None)`.
pub fn decode_request(line: &str) -> Result<Option<Request>, serde_json::Error> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    serde_json::from_str::<Request>(trimmed).map(Some)
}

/// Best-effort id recovery for a line that failed to parse, so the caller can be
/// answered with a correlated error instead of a silent drop.
///
/// A full parse is not enough: the whole point is that the line is malformed. The
/// host drops any response whose id it does not recognise (see the JS sidecar, which
/// returns early on an unknown id and then times out), so an unparseable request must
/// still be answered under the id a tolerant scan can find. Falls back to `null` only
/// when there is no id to find at all.
pub fn peek_id(line: &str) -> Value {
    if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
        if let Some(id) = value.get("id") {
            return id.clone();
        }
    }
    scan_id(line).unwrap_or(Value::Null)
}

/// Find the first top-level-looking `"id"` value without requiring valid JSON.
fn scan_id(line: &str) -> Option<Value> {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    while let Some(found) = line[index..].find("\"id\"") {
        let mut cursor = index + found + 4;
        // skip whitespace, then the colon
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b':' {
            index = cursor;
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return None;
        }
        let rest = &line[cursor..];
        if let Some(text) = rest.strip_prefix('"') {
            // read up to the closing quote, honouring the escapes JSON allows
            let mut value = String::new();
            let mut chars = text.chars();
            while let Some(ch) = chars.next() {
                match ch {
                    '"' => return Some(Value::String(value)),
                    '\\' => {
                        if let Some(escaped) = chars.next() {
                            value.push(match escaped {
                                'n' => '\n',
                                't' => '\t',
                                other => other,
                            });
                        }
                    }
                    other => value.push(other),
                }
            }
            return None;
        }
        let numeric: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_digit() || *ch == '-' || *ch == '+' || *ch == '.' || *ch == 'e' || *ch == 'E')
            .collect();
        if numeric.is_empty() {
            return None;
        }
        if let Ok(int) = numeric.parse::<i64>() {
            return Some(Value::Number(int.into()));
        }
        if let Ok(float) = numeric.parse::<f64>() {
            return serde_json::Number::from_f64(float).map(Value::Number);
        }
        return None;
    }
    None
}

/// `{"id":..,"ok":true,"result":..}`
pub fn ok(id: Value, result: Value) -> Value {
    json!({ "id": id, "ok": true, "result": result })
}

/// `{"id":..,"ok":false,"error":".."}`
pub fn err(id: Value, message: impl Into<String>) -> Value {
    json!({ "id": id, "ok": false, "error": message.into() })
}

/// `call` result envelope: `{ok,name,value,images}`.
pub fn call_result(name: &str, value: Value, images: Vec<Value>) -> Value {
    json!({
        "ok": true,
        "name": name,
        "value": value,
        "images": images
    })
}

/// Pull screenshot bytes out of an MCP tool result and leave everything else as JSON.
///
/// `screenshot` and `get_app_state` return their pixels as an MCP image content
/// block that is a base64 **data URL**. The JSONL contract keeps pixels out of the
/// JSON text ("截图必须拆成独立 image part"): each image becomes
/// `{mimeType, data, name}`, and the accompanying caption stays as a text block
/// when it parses as JSON, or as `{"text": ...}` otherwise.
pub fn pack_call_result(
    result: &crate::rmcp::model::CallToolResult,
) -> (Value, Vec<Value>) {
    let mut images = Vec::new();
    let mut texts = Vec::new();
    let mut index = 0usize;

    for content in &result.content {
        if let Some(image) = content.as_image() {
            let name = format!("screenshot-{index}");
            images.push(json!({
                "mimeType": image.mime_type,
                "data": data_url_payload(&image.data),
                "name": name,
            }));
            index += 1;
        } else if let Some(text) = content.as_text() {
            match serde_json::from_str::<Value>(&text.text) {
                Ok(Value::Object(map)) => texts.push(Value::Object(map)),
                Ok(other) => texts.push(other),
                Err(_) => texts.push(json!({ "text": text.text })),
            }
        }
    }

    if let Some(structured) = result.structured_content.clone() {
        // Structured output is *the* value, screenshot or not.
        //
        // This used to nest the structured payload under a second "value" key whenever
        // the call also carried an image. get_window_state is the only call that does
        // both, so it was the only one affected -- and it was affected on every
        // successful capture: the state object (window, screenshots, accessibility)
        // landed at result.value.value, leaving result.value.screenshots empty while
        // the pixels still rode in result.images. The consumer reads
        // result.value.screenshots, so the model got a screenshot it could not click
        // with and no entry to name it by. Measured on a real session: every visible
        // window returned an empty screenshots array plus one orphaned PNG. The text
        // blocks are dropped here because they are the caption for the same payload
        // (json_result_with_image writes structured.to_string()), never a second,
        // different result.
        return (structured, images);
    }

    let value = match texts.len() {
        0 => Value::Null,
        1 => texts.pop().unwrap_or(Value::Null),
        _ => Value::Array(texts),
    };
    (value, images)
}

/// Strip a `data:<mime>;base64,` prefix. A value that is not a data URL is
/// returned unchanged, so a plain payload is never corrupted.
pub fn data_url_payload(value: &str) -> String {
    if !value.starts_with("data:") {
        return value.to_string();
    }
    value
        .split_once(',')
        .map(|(_, payload)| payload.to_string())
        .unwrap_or_else(|| value.to_string())
}

/// `{"mimeType":"..","data":"..","name":".."}` read back for tests and hosts.
pub fn image_part_name(part: &Value) -> Option<&str> {
    part.get("name").and_then(Value::as_str)
}

pub fn meta_str(meta: &Value, key: &str) -> Option<String> {
    let raw = meta.as_object()?.get(key)?;
    match raw {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub fn meta_i64(meta: &Value, key: &str) -> Option<i64> {
    let raw = meta.as_object()?.get(key)?;
    raw.as_i64()
        .or_else(|| raw.as_u64().map(|n| n as i64))
        .or_else(|| raw.as_f64().map(|n| n as i64))
}

pub fn json_str(map: &Map<String, Value>, key: &str) -> Option<String> {
    match map.get(key)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rmcp::model::{CallToolResult, Content};

    #[test]
    fn decodes_the_documented_envelope() {
        let line = r#"{"id":7,"method":"call","params":{"name":"list_apps","arguments":{}},"meta":{"x-oai-cua-request-budget-ms":9000}}"#;
        let request = decode_request(line).unwrap().unwrap();
        assert_eq!(request.id, json!(7));
        assert_eq!(request.method_name(), "call");
        assert_eq!(json_str(&request.params_object(), "name").as_deref(), Some("list_apps"));
        assert_eq!(request.budget_ms(), 9000);
    }

    #[test]
    fn blank_lines_are_not_requests() {
        assert!(decode_request("   ").unwrap().is_none());
        assert!(decode_request("
").unwrap().is_none());
    }

    #[test]
    fn bad_json_recovers_the_id_for_a_correlated_error() {
        let response = err(peek_id(r#"{"id":3,"method":}"#), "invalid request: expected value");
        assert_eq!(response["id"], json!(3));
        assert_eq!(response["ok"], json!(false));
        assert!(response["error"].as_str().unwrap().starts_with("invalid request"));
    }

    #[test]
    fn a_malformed_line_still_yields_its_id() {
        // The host drops responses whose id it does not recognise, so a parse failure
        // must not cost the caller its correlation.
        assert_eq!(peek_id(r#"{"id":11,"method":}"#), json!(11));
        assert_eq!(peek_id(r#"{"id" : 11 ,"method":}"#), json!(11));
        assert_eq!(peek_id(r#"{"id":"abc","method":}"#), json!("abc"));
        assert_eq!(peek_id(r#"{"id":-4,"method":}"#), json!(-4));
        assert_eq!(peek_id("not json at all"), Value::Null);
        assert_eq!(peek_id(r#"{"method":"health"}"#), Value::Null);
        // A well-formed line keeps using the real parser.
        assert_eq!(peek_id(r#"{"id":12,"method":"health"}"#), json!(12));
    }

    #[test]
    fn budget_defaults_when_meta_is_missing() {
        let request = decode_request(r#"{"id":1,"method":"health"}"#).unwrap().unwrap();
        assert_eq!(request.budget_ms(), DEFAULT_BUDGET_MS);
    }

    #[test]
    fn call_result_is_exactly_ok_name_value_images() {
        let value = call_result("list_apps", json!({"apps": []}), vec![]);
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["images", "name", "ok", "value"]);
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["name"], json!("list_apps"));
    }

    #[test]
    fn screenshot_is_split_into_an_image_part_and_a_json_caption() {
        let result = CallToolResult::success(vec![
            Content::image("QUJDRA==", "image/png"),
            Content::text(r#"{"width":10,"height":20,"source":"portal"}"#),
        ]);
        let (value, images) = pack_call_result(&result);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0]["mimeType"], json!("image/png"));
        assert_eq!(images[0]["data"], json!("QUJDRA=="));
        assert_eq!(images[0]["name"], json!("screenshot-0"));
        // pixels never leak into the JSON text
        assert!(value.to_string().find("QUJDRA==").is_none());
        assert_eq!(value["width"], json!(10));
        assert_eq!(value["source"], json!("portal"));
    }

    #[test]
    fn a_data_url_image_is_unwrapped_to_raw_base64() {
        let result = CallToolResult::success(vec![Content::image(
            "data:image/jpeg;base64,QUJDRA==",
            "image/jpeg",
        )]);
        let (_value, images) = pack_call_result(&result);
        assert_eq!(images[0]["data"], json!("QUJDRA=="));
        assert_eq!(images[0]["mimeType"], json!("image/jpeg"));
    }

    #[test]
    fn non_data_url_payloads_are_left_alone() {
        assert_eq!(data_url_payload("QUJDRA=="), "QUJDRA==");
        assert_eq!(data_url_payload("data:image/png;base64,QUJDRA=="), "QUJDRA==");
    }

    #[test]
    fn a_non_json_caption_is_wrapped_as_text() {
        let result = CallToolResult::success(vec![Content::text("captured 1920x1080")]);
        let (value, images) = pack_call_result(&result);
        assert!(images.is_empty());
        assert_eq!(value["text"], json!("captured 1920x1080"));
    }

    #[test]
    fn structured_output_survives_packing() {
        let mut result = CallToolResult::success(vec![Content::text(r#"{"note":"hi"}"#)]);
        result.structured_content = Some(json!({"apps": [{"id": "x"}]}));
        let (value, _) = pack_call_result(&result);
        assert_eq!(value["apps"][0]["id"], json!("x"));
    }

    /// The caption of a screenshot-bearing call must stay at the top level of `value`.
    ///
    /// `get_window_state` is the only call that sets `structured_content` *and* returns an
    /// image, so it is the only one that used to hit the old "structured plus a screenshot"
    /// branch. That branch nested the state object under a second `value` key, which meant
    /// the consumer read `result.value.screenshots` as `[]` while the pixels still rode in
    /// `result.images` — an image with no entry to click with. Measured on a real session:
    /// every visible window returned `screenshots: []` with one orphaned PNG.
    #[test]
    fn a_screenshot_caption_is_not_nested_under_a_second_value_key() {
        let state = json!({
            "window": {"app": "Fixture", "id": 1},
            "screenshots": [{"id": "0x1:0", "width": 10, "height": 20}],
            "image": {"name": "screenshot-0", "mimeType": "image/png"},
        });
        let mut result = CallToolResult::success(vec![
            Content::text(state.to_string()),
            Content::image("QUJDRA==", "image/png"),
        ]);
        result.structured_content = Some(state.clone());
        let (value, images) = pack_call_result(&result);
        assert!(
            value.get("value").is_none(),
            "the caption must not be nested under a second value key: {value}"
        );
        assert_eq!(value["screenshots"][0]["id"], json!("0x1:0"), "{value}");
        assert_eq!(value["window"]["app"], json!("Fixture"), "{value}");
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn helper_answers_exactly_the_documented_methods() {
        assert_eq!(
            HELPER_METHODS,
            &[
                "health",
                "tools",
                "call",
                "interrupt",
                "shutdown",
                "prompt",
                "end_turn"
            ]
        );
    }
}
