//! Stdio JSON-line protocol matching `computer_use/rpc.py` + `helper_protocol.py`.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const BUDGET_HEADER: &str = "x-oai-cua-request-budget-ms";
pub const APPROVED_APP_META_KEY: &str = "x-oai-cua-approved-app";
pub const TURN_METADATA_KEY: &str = "x-codex-turn-metadata";
pub const TURN_ENDED_EVENT_PREFIX: &str = "Local\\CodexComputerUseTurnEnded-";
pub const TURN_ENDED_MESSAGE: &str = "Computer Use is no longer available in this turn because the turn has ended. Do not call further Computer Use tools in this turn.";
pub const ESCAPE_ERROR: &str = "Computer Use was stopped by the user with the physical Escape key. Stop your work, do not call further Computer Use tools in this turn, and send a final message noting that the user stopped Computer Use.";
pub const BUDGET_EXHAUSTED: &str = "computer-use request budget exhausted";
/// Official rdata 0x131c1c: unknown methods and unknown tools both report
/// `unsupported method: <name>` (the helper never says "unknown method").
pub const UNSUPPORTED_METHOD_PREFIX: &str = "unsupported method: ";
/// Official deserialization channel for host-supplied app approval records.
pub const APP_APPROVAL_REQUIRED: &str = "AppApprovalRequired";
/// DSH meta key carrying the same records when a host cannot nest
/// `AppApprovalRequired` in the request meta.
pub const APPROVAL_GRANTS_META_KEY: &str = "x-oai-cua-approval-grants";

pub const JSONRPC_METHODS: &[&str] = &[
    "health",
    "tools",
    "call",
    "interrupt",
    "shutdown",
    "prompt",
    "end_turn",
    "close",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: Option<String>,
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

    pub fn is_jsonrpc_v2(&self) -> bool {
        self.jsonrpc.as_deref() == Some("2.0")
    }

    /// Official helper line when there is no jsonrpc 2.0 envelope and the method
    /// is a window2 tool rather than a sidecar verb.
    pub fn is_official(&self) -> bool {
        !self.is_jsonrpc_v2() && !JSONRPC_METHODS.contains(&self.method_name())
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
        meta_i64(&self.meta, BUDGET_HEADER).unwrap_or(15_000)
    }
}

pub fn encode_request(
    req_id: i64,
    method: &str,
    params: Value,
    timeout_ms: i64,
    extra_meta: Option<Value>,
) -> String {
    let mut meta = json!({ BUDGET_HEADER: timeout_ms });
    if let Some(Value::Object(extra)) = extra_meta {
        if let Some(obj) = meta.as_object_mut() {
            for (k, v) in extra {
                obj.insert(k, v);
            }
        }
    }
    let payload = json!({
        "id": req_id,
        "method": method,
        "params": if params.is_null() { json!({}) } else { params },
        "meta": meta,
    });
    let mut line = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
    line.push('\n');
    line
}

pub fn decode_response(line: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(line.trim())
}

pub fn decode_request(line: &str) -> Result<Request, serde_json::Error> {
    serde_json::from_str(line.trim())
}

pub fn official_ok(id: Value, result: Value) -> Value {
    json!({ "id": id, "ok": true, "result": result })
}

pub fn official_err(id: Value, error: impl Into<String>, approval: Option<Value>) -> Value {
    let mut body = json!({ "id": id, "ok": false, "error": error.into() });
    if let Some(req) = approval {
        body["approvalRequest"] = req;
    }
    body
}

pub fn jsonrpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn jsonrpc_err(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message.into() });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

pub fn call_result(name: &str, value: Value, images: Vec<Value>) -> Value {
    json!({
        "ok": true,
        "name": name,
        "value": value,
        "images": images
    })
}

/// Official `AppApprovalRequest` (helper .rdata): four fields, with
/// `riskLevel` a two-value enum. Password managers and antivirus/security
/// products classify as `high`.
///
/// `allow_persistent` is not a constant: the official JS layer refuses to offer
/// "always" for audio (`helper_transport.js:139` `allowPersistentApproval && !isAudio`),
/// so the audio approval request carries `false` (APS-12).
pub fn approval_request(app: &str, display_name: &str, allow_persistent: bool) -> Value {
    json!({
        "app": app,
        "displayName": display_name,
        "riskLevel": crate::policy::risk_level(app),
        "allowPersistentApproval": allow_persistent,
    })
}

/// Official helper input record (APS-02): the exe contains a **Deserialize**
/// implementation for
/// `AppApprovalRequired{request: <app> -> AppApprovalRequest{display_name, risk_level,
/// allow_persistent_approval}}` (`all_functions.c:4661/4693-4695`). The app is the
/// map key, so the value shape carries no `app` field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalGrant {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub risk_level: Option<String>,
    #[serde(default)]
    pub allow_persistent_approval: bool,
}

/// Parse one official `AppApprovalRequired` record, accepting both the exe's
/// snake_case shape and the camelCase shape the helper serializes.
/// Returns `(app, grant)`.
pub fn parse_app_approval_required(value: &Value) -> Option<(String, ApprovalGrant)> {
    let outer = value.get(APP_APPROVAL_REQUIRED).unwrap_or(value);
    let request = outer.get("request").unwrap_or(outer);
    let map = request.as_object()?;
    // camelCase object form: {"app": ..., "displayName": ..., "riskLevel": ...}
    if let Some(app) = map.get("app").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        let display_name = map
            .get("displayName")
            .or_else(|| map.get("display_name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let risk_level = map
            .get("riskLevel")
            .or_else(|| map.get("risk_level"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let allow_persistent_approval = map
            .get("allowPersistentApproval")
            .or_else(|| map.get("allow_persistent_approval"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return Some((
            app.to_string(),
            ApprovalGrant { display_name, risk_level, allow_persistent_approval },
        ));
    }
    // official form: { "<app>": {display_name, risk_level, allow_persistent_approval} }
    for (app, body) in map {
        let Some(record) = body.as_object() else { continue };
        if !record.contains_key("display_name")
            && !record.contains_key("risk_level")
            && !record.contains_key("allow_persistent_approval")
        {
            continue;
        }
        let Ok(grant) = serde_json::from_value::<ApprovalGrant>(body.clone()) else { continue };
        return Some((app.clone(), grant));
    }
    None
}

/// Pull every approval grant out of a request `meta`: either a bare
/// `AppApprovalRequired` member or an array/object under
/// `x-oai-cua-approval-grants`.
pub fn approval_grants_from_meta(meta: &Value) -> Vec<(String, ApprovalGrant)> {
    if let Some(found) = parse_app_approval_required(meta) {
        return vec![found];
    }
    match meta.get(APPROVAL_GRANTS_META_KEY) {
        Some(Value::Array(items)) => items.iter().filter_map(parse_app_approval_required).collect(),
        Some(other) => parse_app_approval_required(other).into_iter().collect(),
        None => Vec::new(),
    }
}

/// Build an `AppApprovalRequest` from a host-supplied grant record instead of
/// re-deriving `displayName`/`riskLevel`/`allowPersistentApproval`.
pub fn approval_request_with_grant(app: &str, grant: &ApprovalGrant) -> Value {
    let display = if grant.display_name.trim().is_empty() { app } else { grant.display_name.trim() };
    json!({
        "app": app,
        "displayName": display,
        "riskLevel": grant.risk_level.as_deref().unwrap_or("low"),
        "allowPersistentApproval": grant.allow_persistent_approval,
    })
}

/// Official helper `AppApprovalRequired` message: the helper could not proceed
/// without an approval for this app, so the client must elicit one.
pub const APPROVAL_REQUIRED_PREFIX: &str = "Computer Use requires approval to use ";

#[derive(Debug)]
pub enum ErrorKind {
    Parse,
    InvalidRequest,
    UnknownMethod,
    Type,
    Value,
    Desktop,
    Approval,
    Interrupt,
    Other,
}

#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    pub approval: Option<Value>,
    /// P1: machine-readable refinement ("window_gone", "desktop_locked", "input_monitor")
    /// set only at the sites JS classifies; None everywhere else.
    pub code: Option<&'static str>,
}

impl Error {
    pub fn parse(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Parse, message: msg.into(), approval: None, code: None }
    }
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::InvalidRequest, message: msg.into(), approval: None, code: None }
    }
    pub fn unknown_method(method: impl std::fmt::Display) -> Self {
        Self {
            kind: ErrorKind::UnknownMethod,
            message: format!("{UNSUPPORTED_METHOD_PREFIX}{method}"),
            approval: None,            code: None,
        }
    }
    pub fn unknown_tool(name: impl std::fmt::Display) -> Self {
        Self {
            kind: ErrorKind::UnknownMethod,
            message: format!("{UNSUPPORTED_METHOD_PREFIX}{name}"),
            approval: None,            code: None,
        }
    }
    pub fn key(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::UnknownMethod, message: msg.into(), approval: None, code: None }
    }
    pub fn type_err(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Type, message: msg.into(), approval: None, code: None }
    }
    pub fn value(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Value, message: msg.into(), approval: None, code: None }
    }
    pub fn desktop(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Desktop, message: msg.into(), approval: None, code: None }
    }
    pub fn other(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Other, message: msg.into(), approval: None, code: None }
    }
    pub fn interrupt() -> Self {
        Self { kind: ErrorKind::Interrupt, message: ESCAPE_ERROR.into(), approval: None, code: None }
    }
    pub fn turn_ended() -> Self {
        Self { kind: ErrorKind::Interrupt, message: TURN_ENDED_MESSAGE.into(), approval: None, code: None }
    }
    pub fn approval(app: &str) -> Self {
        if app == AUDIO_APP {
            return Self {
                kind: ErrorKind::Approval,
                message: AUDIO_APPROVAL_MESSAGE.into(),
                // APS-12: the official JS layer never offers "always" for audio.
                approval: Some(approval_request(AUDIO_APP, AUDIO_DISPLAY, false)),
                code: None,
            };
        }
        // The helper's own message is the official AppApprovalRequired sentence;
        // the *user-facing* question is built by the approval UI
        // ("Allow DeepSeek Harness to use <name>?" — D2 branding).
        Self {
            kind: ErrorKind::Approval,
            message: format!("{APPROVAL_REQUIRED_PREFIX}{app}"),
            approval: Some(approval_request(app, app, true)),
            code: None,
        }
    }

    /// Official transport behaviour when the user declines the app approval:
    /// `Computer Use was not approved to use <displayName>`.
    pub fn approval_denied(display_name: &str) -> String {
        format!("Computer Use was not approved to use {display_name}")
    }

    /// Official launch_app gate when the resolved catalog/exe target is not in the
    /// approved set. The approval request carries the user-visible app name so the
    /// prompt can say "Allow DeepSeek Harness to use Notepad?" rather than an id.
    pub fn launch_unapproved(app: &str, display_name: &str) -> Self {
        let display = if display_name.trim().is_empty() { app } else { display_name.trim() };
        Self {
            kind: ErrorKind::Approval,
            message: LAUNCH_APP_NO_APPROVED.into(),
            approval: Some(approval_request(app, display, true)),
            code: None,
        }
    }

    /// APS-11: same gate as `launch_unapproved`, but `riskLevel` is also derived
    /// from the PE identity, so a randomly named antivirus executable still
    /// reports `high`. `main.rs::launch_app` holds `info.product_name` /
    /// `info.original_filename` and should call this instead of
    /// `launch_unapproved`.
    pub fn launch_unapproved_identity(
        app: &str,
        display_name: &str,
        product_name: Option<&str>,
        original_filename: Option<&str>,
    ) -> Self {
        let display = if display_name.trim().is_empty() { app } else { display_name.trim() };
        let mut request = approval_request(app, display, true);
        request["riskLevel"] =
            json!(crate::policy::risk_level_for(app, product_name, original_filename));
        Self {
            kind: ErrorKind::Approval,
            message: LAUNCH_APP_NO_APPROVED.into(),
            approval: Some(request),
            code: None,
        }
    }

    /// Coarse machine-readable kind ("desktop", "approval", ...), for the error body.
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            ErrorKind::Parse => "parse",
            ErrorKind::InvalidRequest => "invalid_request",
            ErrorKind::UnknownMethod => "unknown_method",
            ErrorKind::Type => "type",
            ErrorKind::Value => "value",
            ErrorKind::Desktop => "desktop",
            ErrorKind::Approval => "approval",
            ErrorKind::Interrupt => "interrupt",
            ErrorKind::Other => "other",
        }
    }

    /// P1: attach the fine-grained machine code used by the JS error classifier.
    pub fn coded(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    pub fn to_response(&self, id: Value, jsonrpc_v2: bool) -> Value {
        match self.kind {
            ErrorKind::Parse => jsonrpc_err(id, -32700, &self.message, None),
            ErrorKind::InvalidRequest => jsonrpc_err(id, -32600, &self.message, None),
            _ if jsonrpc_v2 => {
                let code = match self.kind {
                    ErrorKind::UnknownMethod => -32601,
                    ErrorKind::Approval => -32001,
                    _ => -32000,
                };
                let data = self.approval.as_ref().map(|req| json!({ "approvalRequest": req }));
                jsonrpc_err(id, code, &self.message, data)
            }
            _ => {
                // P1: structured error classification. `kind` is the coarse ErrorKind;
                // `code` is the fine refinement set at specific creation sites. JS reads
                // code first and falls back to legacy message matching.
                let mut body = official_err(id, &self.message, self.approval.clone());
                body["kind"] = json!(self.kind_name());
                if let Some(code) = self.code {
                    body["code"] = json!(code);
                }
                body
            }
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub fn meta_str(meta: &Value, key: &str) -> Option<String> {
    let obj = meta.as_object()?;
    let raw = obj.get(key)?;
    match raw {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub fn meta_i64(meta: &Value, key: &str) -> Option<i64> {
    let obj = meta.as_object()?;
    let raw = obj.get(key)?;
    raw.as_i64().or_else(|| raw.as_u64().map(|n| n as i64)).or_else(|| raw.as_f64().map(|n| n as i64))
}

pub fn json_str(map: &Map<String, Value>, key: &str) -> Option<String> {
    let raw = map.get(key)?;
    match raw {
        Value::Null => None,
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        }
        other => {
            let t = other.to_string();
            if t == "null" { None } else { Some(t.trim_matches('"').to_string()) }
        }
    }
}

pub fn json_f64(map: &Map<String, Value>, key: &str) -> Result<Option<f64>, Error> {
    let Some(raw) = map.get(key) else { return Ok(None) };
    if raw.is_null() {
        return Ok(None);
    }
    match raw {
        Value::Number(n) => n.as_f64().map(Some).ok_or_else(|| Error::type_err(format!("{key} must be a finite number"))),
        Value::String(s) => s.parse::<f64>().map(Some).map_err(|_| Error::type_err(format!("{key} must be a finite number"))),
        Value::Bool(_) => Err(Error::type_err(format!("{key} must be a finite number"))),
        _ => Err(Error::type_err(format!("{key} must be a finite number"))),
    }
}

pub fn json_i64(map: &Map<String, Value>, key: &str) -> Result<Option<i64>, Error> {
    let Some(raw) = map.get(key) else { return Ok(None) };
    if raw.is_null() {
        return Ok(None);
    }
    let number = match raw {
        Value::Number(n) => n.as_f64().ok_or_else(|| Error::type_err(format!("{key} must be an integer")))?,
        Value::String(s) if !s.trim().is_empty() => s.parse::<f64>().map_err(|_| Error::type_err(format!("{key} must be an integer")))?,
        _ => return Err(Error::type_err(format!("{key} must be an integer"))),
    };
    if !number.is_finite() || number < 0.0 {
        return Err(Error::type_err(format!("{key} must be >= 0")));
    }
    Ok(Some(number as i64))
}

pub fn json_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64().or_else(|| n.as_i64().map(|v| v as u64)).or_else(|| n.as_f64().map(|v| v as u64)),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

pub fn require_text(map: &Map<String, Value>, key: &str) -> Result<String, Error> {
    match map.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(Error::type_err(format!("{key} is required"))),
    }
}

/// Official `set_value.value` / `type_text.text` are type-checked only
/// (`typeof value !== 'string'`), so `""` is legal and clears the field.
/// `launch_app.app` and `press_key.key` keep `require_text` because the official
/// layer treats a falsy value as missing.
pub fn require_text_allow_empty(map: &Map<String, Value>, key: &str) -> Result<String, Error> {
    match map.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(Error::type_err(format!("{key} is required"))),
    }
}

pub const AUDIO_APP: &str = "computer-audio";
pub const AUDIO_DISPLAY: &str = "computer audio";
pub const AUDIO_APPROVAL_MESSAGE: &str = "Allow Computer Use to record computer audio?";
pub const LAUNCH_APP_NO_APPROVED: &str = "launch_app has no approved target";

/// Official `struct StartAudioRecordingParams with 1 element`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StartAudioRecordingParams {
    #[serde(default)]
    pub max_duration_ms: Option<i64>,
}

impl StartAudioRecordingParams {
    pub const DEFAULT_MS: i64 = 60_000;
    pub const MIN_MS: i64 = 1;
    pub const MAX_MS: i64 = 300_000;

    pub fn from_map(map: &Map<String, Value>) -> Result<Self, Error> {
        Ok(Self {
            max_duration_ms: json_i64(map, "max_duration_ms")?,
        })
    }

    /// Recording duration used by `start_audio_recording` (default 60000).
    pub fn duration_ms(&self) -> Result<u64, Error> {
        let ms = self.max_duration_ms.unwrap_or(Self::DEFAULT_MS);
        if ms < Self::MIN_MS || ms > Self::MAX_MS {
            return Err(Error::value(format!(
                "max_duration_ms must be from {} through {}",
                Self::MIN_MS, Self::MAX_MS
            )));
        }
        Ok(ms as u64)
    }
}

/// Python `windows_backend.observe`: `optional_int(..., "max_duration_ms") or 800`.
pub fn capture_timeout_ms(map: &Map<String, Value>) -> Result<u32, Error> {
    Ok(json_i64(map, "max_duration_ms")?
        .filter(|ms| *ms > 0)
        .unwrap_or(800) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_unapproved_uses_official_message() {
        let err = Error::launch_unapproved("mspaint.exe", "Paint");
        assert_eq!(err.message, LAUNCH_APP_NO_APPROVED);
        assert!(err.approval.is_some());
        let approval = err.approval.as_ref().unwrap();
        assert_eq!(approval["app"], "mspaint.exe");
        // The prompt shows the user-visible name, not the raw identifier.
        assert_eq!(approval["displayName"], "Paint");
    }

    #[test]
    fn launch_unapproved_falls_back_to_the_app_id() {
        // An empty or blank display name must not produce an empty prompt.
        for blank in ["", "   "] {
            let err = Error::launch_unapproved("mspaint.exe", blank);
            let approval = err.approval.as_ref().unwrap();
            assert_eq!(approval["displayName"], "mspaint.exe");
        }
    }

    #[test]
    fn approval_risk_level_marks_sensitive_apps_high() {
        let err = Error::approval("1Password.exe");
        assert_eq!(err.approval.as_ref().unwrap()["riskLevel"], "high");
        let err = Error::approval("notepad.exe");
        assert_eq!(err.approval.as_ref().unwrap()["riskLevel"], "low");
    }

    #[test]
    fn unsupported_method_is_the_official_string() {
        // TC-14: official rdata 0x131c1c is "unsupported method: ".
        assert_eq!(Error::unknown_method("frobnicate").message, "unsupported method: frobnicate");
        assert_eq!(Error::unknown_tool("frobnicate").message, "unsupported method: frobnicate");
        assert_eq!(UNSUPPORTED_METHOD_PREFIX, "unsupported method: ");
    }

    #[test]
    fn empty_value_and_text_are_accepted_app_and_key_are_not() {
        // TC-11: official only type-checks set_value.value / type_text.text, so
        // "" clears the field; launch_app.app and press_key.key stay required.
        let mut map = Map::new();
        map.insert("value".into(), Value::String(String::new()));
        assert_eq!(require_text_allow_empty(&map, "value").unwrap(), "");
        assert_eq!(require_text(&map, "value").unwrap_err().message, "value is required");
        let mut key = Map::new();
        key.insert("key".into(), Value::String("   ".into()));
        assert_eq!(require_text(&key, "key").unwrap(), "   ");
        assert_eq!(require_text_allow_empty(&key, "missing").unwrap_err().message, "missing is required");
        map.insert("value".into(), Value::Number(1.into()));
        assert_eq!(require_text_allow_empty(&map, "value").unwrap_err().message, "value is required");
    }

    #[test]
    fn audio_approval_never_offers_persistence() {
        // APS-12: official JS forces persist ["session"] for audio regardless of
        // what the helper returns, and the helper must not promise "always".
        let audio = Error::approval(AUDIO_APP);
        assert_eq!(audio.approval.as_ref().unwrap()["allowPersistentApproval"], false);
        let app = Error::approval("notepad.exe");
        assert_eq!(app.approval.as_ref().unwrap()["allowPersistentApproval"], true);
    }

    #[test]
    fn official_app_approval_required_record_is_parsed() {
        // APS-02: the exe deserializes this snake_case record.
        let record = json!({
            "AppApprovalRequired": {
                "request": {
                    "mspaint.exe": {
                        "display_name": "Paint",
                        "risk_level": "low",
                        "allow_persistent_approval": true
                    }
                }
            }
        });
        let (app, grant) = parse_app_approval_required(&record).expect("grant");
        assert_eq!(app, "mspaint.exe");
        assert_eq!(grant.display_name, "Paint");
        assert_eq!(grant.risk_level.as_deref(), Some("low"));
        assert!(grant.allow_persistent_approval);
        let request = approval_request_with_grant(&app, &grant);
        assert_eq!(request["displayName"], "Paint");
        assert_eq!(request["riskLevel"], "low");
        assert_eq!(request["allowPersistentApproval"], true);
    }

    #[test]
    fn camel_app_approval_record_is_parsed_too() {
        let record = json!({
            "app": "1Password.exe",
            "displayName": "1Password",
            "riskLevel": "high",
            "allowPersistentApproval": false
        });
        let (app, grant) = parse_app_approval_required(&record).expect("grant");
        assert_eq!(app, "1Password.exe");
        assert_eq!(grant.display_name, "1Password");
        assert_eq!(grant.risk_level.as_deref(), Some("high"));
        assert!(!grant.allow_persistent_approval);
    }

    #[test]
    fn launch_unapproved_identity_raises_the_risk_level() {
        let err = Error::launch_unapproved_identity("foo.exe", "Foo", Some("Kaspersky Internet Security"), None);
        assert_eq!(err.approval.as_ref().unwrap()["riskLevel"], "high");
        let err = Error::launch_unapproved_identity("foo.exe", "Foo", Some("Notepad"), None);
        assert_eq!(err.approval.as_ref().unwrap()["riskLevel"], "low");
        assert_eq!(err.approval.as_ref().unwrap()["displayName"], "Foo");
    }

    #[test]
    fn approval_grants_are_read_from_meta() {
        let meta = json!({
            "x-oai-cua-approval-grants": [
                {"AppApprovalRequired": {"request": {"a.exe": {"display_name": "A", "allow_persistent_approval": true}}}},
                {"AppApprovalRequired": {"request": {"b.exe": {"display_name": "B", "allow_persistent_approval": false}}}}
            ]
        });
        let grants = approval_grants_from_meta(&meta);
        assert_eq!(grants.len(), 2);
        assert_eq!(grants[0].0, "a.exe");
        assert_eq!(grants[1].1.display_name, "B");
        assert!(approval_grants_from_meta(&json!({})).is_empty());
    }
}





