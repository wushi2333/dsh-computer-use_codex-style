//! JSONL helper dispatcher: the plugin protocol on top of the crate's MCP tool handlers.
//!
//! helper-linux is a fork of ilysenko/codex-desktop-linux's computer-use-linux crate,
//! whose entry point is an MCP server (server::serve_mcp). The crate's tool handlers are
//! reused verbatim; this module replaces the *transport* with the plugin's stdio JSONL
//! protocol and exposes exactly the seven sky.window-style tools the plugin's Linux
//! surface declares.
//!
//! The dispatcher drives the crate's own ToolRouter rather than re-serializing its
//! logic: schemas, state and backend selection stay single-sourced in server.rs.

use std::io::Write as _;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use rmcp::model::CallToolResult;

use crate::protocol::{self, HELPER_METHODS};
use crate::server::ComputerUseLinux;

/// The upstream commit this fork was cut from.
pub const UPSTREAM_REPO: &str = "https://github.com/ilysenko/codex-desktop-linux";
pub const UPSTREAM_COMMIT: &str = "5f7310d71dd02e6e0131deec6fa89d26c8bcaf9c";
pub const UPSTREAM_CRATE_VERSION: &str = "0.4.9-linux-alpha1";

/// The plugin's Linux tool surface: exactly these seven, in the official Linux
/// (crate-native) names and parameter shapes. Every other native MCP tool --
/// doctor, setup_accessibility, setup_window_targeting, list_windows, focused_window,
/// activate_window, perform_action, set_value, drag, move_window, resize_window --
/// is deliberately neither listed nor routable.
pub const SURFACE_TOOLS: &[&str] = &[
    "list_apps",
    "get_app_state",
    "screenshot",
    "click",
    "scroll",
    "press_key",
    "type_text",
];

/// The API reference shipped as the prompt response.
const API_PROMPT: &str = include_str!("../assets/prompts/api.md");

/// Why an in-flight call was stopped, so the host is told the truth rather than
/// receiving a generic failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// The user/host called interrupt.
    Interrupted,
    /// The turn ended while a call was still running.
    TurnEnded,
    /// The helper is going away.
    Shutdown,
}


/// Mutable helper state, shared between the reader, the painter and any in-flight call.
#[derive(Debug, Default)]
pub struct HelperState {
    /// Set by interrupt, cleared by end_turn. While set, work methods fail with the
    /// same sentence the official helper uses so the model stops driving the desktop.
    interrupted: AtomicBool,
    /// Set by shutdown; the read loop stops once the response has been flushed.
    shutdown: AtomicBool,
}

impl HelperState {
    pub fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::SeqCst)
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

}

/// How a cancelled call explains itself, matching the official helper wording.
fn cancel_message(reason: CancelReason) -> String {
    match reason {
        CancelReason::Interrupted => protocol::INTERRUPTED_MESSAGE.to_string(),
        CancelReason::TurnEnded => protocol::TURN_ENDED_MESSAGE.to_string(),
        CancelReason::Shutdown => "computer-use helper is shutting down".to_string(),
    }
}

/// The X11 experience layer (status pill, synthesized pointer, freshness lease, global
/// Escape), or None when this session has none to offer.
///
/// Created lazily on the first turn instead of at process start: a helper that is spawned
/// to answer health on a Wayland desktop must not pay for an X connection, and a failure
/// to open one is a supported state rather than an error the tool surface has to carry.
/// helper-rs owns the same four behaviours on Windows; this is the X11 twin.
fn experience() -> Option<std::sync::Arc<crate::x11_experience::Session>> {
    crate::x11_experience::session()
}

/// How a tool call relates to the experience layer.
///
/// The X11 layer arms on *observation*, which is this crate's own documented rule
/// ("Observation begins for this turn"): the pill comes up, the synthesized pointer takes
/// the desktop over, the Escape grab is installed and the freshness lease starts watching
/// for human input. The Windows helper arms from the overlay's show(), which only its
/// input methods call; keeping the X11 rule here is deliberate and is recorded in
/// helper-linux/README.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExperienceUse {
    /// Shows the operator what is happening. Arms the layer.
    Observe,
    /// Changes the desktop, so the pill says "working" while it runs.
    Act,
    /// Neither: listing windows or apps only reads a table.
    None,
}

/// Classify a surface tool by name, whatever surface it arrived on.
///
/// A name can be served by either surface (click/press_key/type_text/scroll are on both),
/// and the answer is the same either way: the layer is per X session, not per surface.
pub(crate) fn experience_use(name: &str) -> ExperienceUse {
    match name {
        // Observation: the calls whose result the human is being shown.
        "get_app_state" | "screenshot" | "get_window_state" => ExperienceUse::Observe,
        // Table reads: real work, but nothing is shown and nothing is driven.
        "list_apps" | "list_windows" | "get_window" => ExperienceUse::None,
        // read-only table reads stay above; everything else, launch_app included, acts on
        // the desktop and arms the experience layer.
        _ => ExperienceUse::Act,
    }
}

/// Arm the layer for a call, if this is a call that arms it.
///
/// `begin()` is idempotent (arming twice does not restart the grace window), so an
/// observation followed by a click in the same turn produces one armed turn, not two.
///
/// Arming is best effort on purpose. The layer is created lazily, so a session that
/// cannot offer one (Wayland, no DISPLAY, a refused X connection, a refused Escape grab)
/// simply reports that through health/diagnostics and the call runs anyway. Every Handle
/// method here is non-blocking and drops a dead X thread's command instead of failing, so
/// no arming step can fail a tool call or change what a call returns.
pub(crate) fn begin_experience(session: Option<&crate::x11_experience::Session>, use_: ExperienceUse) {
    let Some(session) = session else { return };
    if use_ == ExperienceUse::None {
        return;
    }
    session.begin();
    if use_ == ExperienceUse::Observe {
        session.observe();
    } else {
        session.working();
    }
}

/// Run one tool call on whichever surface serves it.
///
/// Deliberately not an async fn: it is the arming that has to be synchronously visible to
/// the caller (the Escape branch is only armed when this call really armed the layer), so
/// arming stays in the caller and this only routes and runs.
async fn invoke_experienced(
    service: &ComputerUseLinux,
    name: &str,
    arguments: Map<String, Value>,
    budget_ms: i64,
    window2_native: bool,
) -> Result<CallToolResult, String> {
    if window2_native {
        // The window2 handlers are synchronous (they talk to the X server directly), so
        // they run on the blocking pool and never on the request-loop thread.
        let dispatched = name.to_string();
        return tokio::task::spawn_blocking(move || {
            crate::x11::window2::dispatch(&dispatched, arguments)
        })
        .await
        .unwrap_or_else(|error| Err(format!("the window2 worker did not finish: {error}")));
    }
    service.invoke_surface_tool(name, arguments, budget_ms).await
}

/// A method that must stay reachable while the helper is interrupted, so the host can
/// always clean up. Mirrors the official lifecycle-method carve-out.
fn is_lifecycle_method(method: &str) -> bool {
    matches!(
        method,
        "health" | "tools" | "prompt" | "interrupt" | "shutdown" | "end_turn"
    )
}

/// Serve the plugin protocol on stdin/stdout until EOF, shutdown, or a stopped helper.
pub async fn serve() -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    serve_stream(stdin, stdout).await
}

/// Transport-agnostic core, so tests can drive it over an in-memory duplex.
///
/// The reader and the worker are separate tasks on purpose. The worker runs one request at
/// a time and in order -- desktop control is inherently sequential, and running a screenshot
/// concurrently with a click would race the user's own desktop. The reader stays responsive
/// anyway, which is what makes `interrupt` able to stop the call the worker is currently
/// running: a single loop that awaited each call would still be parked on the previous one
/// when the interrupt line arrived, so it could only ever cancel work that had already
/// finished.
///
/// Ordering is preserved, and because the two tasks are joined through one channel, each
/// request is judged against the state at *its own* point in the stream. Cancelling a call
/// therefore cannot retroactively cancel a request that arrived before the cancellation.
pub async fn serve_stream<R, W>(reader: R, writer: W) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Request>();
    let worker = tokio::spawn(work(rx, writer));
    let reader_result = read_requests(reader, &tx).await;
    // Closing the channel lets the worker finish the request it is running (including
    // shutdown) and flush stdout before this returns.
    drop(tx);
    let work_result = worker.await;

    reader_result?;
    work_result??;
    Ok(())
}

/// One unit of work handed from the reader to the worker.
///
/// A line that failed to parse is still carried, with the id recovered from it: the host
/// drops any response it cannot correlate, so a dropped line would make it wait out its
/// whole timeout instead of failing fast.
enum Request {
    Call(protocol::Request),
    Malformed { id: Value, error: String },
}

/// Read lines and forward every one of them, including the malformed ones, in order.
///
/// This task never waits on a tool call, so an `interrupt` written while a screenshot is
/// running reaches `work` immediately.
async fn read_requests<R>(
    reader: R,
    tx: &tokio::sync::mpsc::UnboundedSender<Request>,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request = match protocol::decode_request(&line) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(error) => {
                // Keep the line so the worker can answer it under the id it names; the
                // host drops responses it cannot correlate, and a silent drop would make
                // it wait out its whole timeout.
                let _ = tx.send(Request::Malformed {
                    id: protocol::peek_id(&line),
                    error: error.to_string(),
                });
                continue;
            }
        };
        if tx.send(Request::Call(request)).is_err() {
            break;
        }
    }
    Ok(())
}

/// Run requests one at a time, in order, writing each response before starting the next.
///
/// `pending` is the push-front queue the interrupt path needs: a non-cancellable request
/// that arrives while a call is running must be answered *after* that call, in its original
/// order, and an unbounded channel has no way to put it back at the front.
async fn work<W>(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Request>,
    mut writer: W,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let service = ComputerUseLinux::default();
    let state = Arc::new(HelperState::default());
    let mut pending: std::collections::VecDeque<Request> = std::collections::VecDeque::new();

    loop {
        let item = match pending.pop_front() {
            Some(item) => Some(item),
            None => rx.recv().await,
        };
        let Some(item) = item else { break };

        let request = match item {
            Request::Malformed { id, error } => {
                let response = protocol::err(id, format!("invalid request: {error}"));
                write_line(&mut writer, &response).await?;
                continue;
            }
            Request::Call(request) => request,
        };

        let id = request.id.clone();
        let method = request.method_name().to_string();
        let params = request.params_object();

        if !HELPER_METHODS.contains(&method.as_str()) {
            let response = protocol::err(
                id,
                format!("{}{}", protocol::UNSUPPORTED_METHOD_PREFIX, method),
            );
            write_line(&mut writer, &response).await?;
            continue;
        }

        if is_lifecycle_method(&method) {
            let (result, shutdown) = dispatch_verb(&service, &state, &method, &params).await;
            let response = match result {
                Ok(value) => protocol::ok(id, value),
                Err(error) => protocol::err(id, error),
            };
            write_line(&mut writer, &response).await?;
            if shutdown {
                // Terminal: answer what the host already sent so nothing is left hanging,
                // but run none of it.
                for abandoned in drain_as_abandoned(&mut rx, &mut pending).await {
                    write_line(&mut writer, &abandoned).await?;
                }
                break;
            }
            continue;
        }

        if state.is_interrupted() {
            let response = protocol::err(id, protocol::INTERRUPTED_MESSAGE);
            write_line(&mut writer, &response).await?;
            continue;
        }

        // The stopped call (if a cancelling verb arrived) and that verb's reply, in the
        // order the host must see them.
        let responses = run_interruptible(
            &service,
            &state,
            id.clone(),
            &params,
            request.budget_ms(),
            &mut rx,
            &mut pending,
        )
        .await;
        for response in &responses {
            write_line(&mut writer, response).await?;
        }
    }
    Ok(())
}


/// Frame a finished call the way `call` promises: `{ok,name,value,images}` on success,
/// `{ok:false,error}` otherwise, always under the id the host is waiting on.
fn pack_outcome(id: Value, name: &str, outcome: Result<CallToolResult, String>) -> Value {
    match outcome {
        Err(message) => protocol::err(id, message),
        Ok(result) => {
            let (value, images) = protocol::pack_call_result(&result);
            protocol::ok(id, protocol::call_result(name, value, images))
        }
    }
}

/// Run one tool call while still listening for the verbs that can stop it.
///
/// `interrupt`, `end_turn` and `shutdown` act the moment they arrive instead of after the
/// current call finishes -- otherwise `interrupt` could only ever stop work that had already
/// completed. Anything else is pushed to `pending` so it is answered afterwards, in order.
///
/// Returns the responses to write, in exactly the order the host must receive them.
///
/// A cancelling verb produces two: the call it stopped (under the call's own id, first --
/// the host is waiting on that id and saw nothing else yet) and then the verb's own reply.
/// Anything that is not a cancelling verb goes to `pending` and is answered afterwards, in
/// order, by the caller.
#[allow(clippy::too_many_arguments)]
async fn run_interruptible(
    service: &ComputerUseLinux,
    state: &Arc<HelperState>,
    call_id: Value,
    params: &Map<String, Value>,
    budget_ms: i64,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Request>,
    pending: &mut std::collections::VecDeque<Request>,
) -> Vec<Value> {
    let scope = Arc::clone(state);
    // `params` carries the `call` envelope: the tool name and its arguments.
    let tool_name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let arguments = match params.get("arguments") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };
    if let Some(refusal) = surface_guard(&call_id, &tool_name) {
        return vec![refusal];
    }
    // A call carries the surface it belongs to when the turn is on a window2 face, so a
    // name that both surfaces define (list_apps/click/press_key/type_text/scroll) resolves
    // to the window2 handler there instead of the P1 sky.window one. With no tag the old
    // routing stands, which keeps a P1 host bit-for-bit compatible.
    let call_surface = protocol::json_str(params, "surface");
    // Both surfaces arm the same layer: the experience layer is one per X session, and the
    // plugin's default Linux surface is the P1 one, so a layer that only armed on window2
    // faces would never arm on the default path. Arming is observation-driven and never
    // changes what a call returns.
    //
    // The surface decides only *what runs*: the window2 handlers must not go through the
    // crate's MCP router, which knows nothing about window-relative coordinates.
    let window2_native = is_window2_native_call(&tool_name, call_surface.as_deref());
    // The layer decides what "observing" means for this name and arms *synchronously*,
    // before the call future exists: the Escape branch below is only live when this call
    // really armed the layer, and a future's body has not run at that point.
    let escape_session = experience();
    begin_experience(escape_session.as_deref(), experience_use(&tool_name));
    let escape_armed = escape_session.as_ref().is_some_and(|session| session.is_armed());
    let escape_notice = match escape_session.as_ref() {
        Some(session) => session.interrupt_notice(),
        None => Arc::new(tokio::sync::Notify::new()),
    };
    let call = invoke_experienced(service, &tool_name, arguments, budget_ms, window2_native);
    tokio::pin!(call);

    loop {
        tokio::select! {
            result = &mut call => return vec![pack_outcome(call_id, &tool_name, result)],
            // A physical Escape is out-of-band. helper-rs cancels through its low-level
            // hook (exit status 130); here the notification comes from the X thread that
            // saw the Escape -- through the root grab when it could take one, and through
            // the XInput2 raw-key path when the grab was refused, which is the case on a
            // desktop whose compositor already owns the Escape binding.
            //
            // The branch is armed only when this call really armed the layer, so a call
            // that leaves the layer alone (list_windows, or a session with no layer at
            // all) keeps the old two-way select exactly.
            _ = escape_notice.notified(), if escape_armed => {
                if let Some(session) = escape_session.as_ref() {
                    let _ = session.take_escaped();
                }
                return vec![
                    protocol::err(call_id.clone(), cancel_message(CancelReason::Interrupted)),
                ];
            }
            item = rx.recv() => {
                let Some(item) = item else {
                    // The host closed the stream: let the call finish so its result is not lost.
                    let result = call.await;
                    return vec![pack_outcome(call_id, &tool_name, result)];
                };
                let request = match item {
                    Request::Malformed { .. } => {
                        pending.push_back(item);
                        continue;
                    }
                    Request::Call(request) => request,
                };
                let verb = request.method_name().to_string();
                if !is_lifecycle_method(&verb) {
                    pending.push_back(Request::Call(request));
                    continue;
                }
                let (result, shutdown) =
                    dispatch_verb(service, &scope, &verb, &request.params_object()).await;
                let verb_response = match result {
                    Ok(value) => protocol::ok(request.id.clone(), value),
                    Err(error) => protocol::err(request.id.clone(), error),
                };
                match verb.as_str() {
                    "interrupt" | "shutdown" | "end_turn" => {
                        let reason = match verb.as_str() {
                            "interrupt" => CancelReason::Interrupted,
                            "shutdown" => CancelReason::Shutdown,
                            _ => CancelReason::TurnEnded,
                        };
                        let stopped = protocol::err(call_id.clone(), cancel_message(reason));
                        if shutdown {
                            // Responses go back in request order: the call that was stopped,
                            // then anything the host sent before the shutdown arrived (answered
                            // as abandoned), and only then shutdown's own reply.
                            let mut responses = vec![stopped];
                            responses.extend(drain_as_abandoned(rx, pending).await);
                            responses.push(verb_response);
                            return responses;
                        }
                        // interrupt / end_turn: the stopped call, then the verb's own reply.
                        return vec![stopped, verb_response];
                    }
                    // health/tools/prompt are not cancelling; answer them after the call.
                    _ => pending.push_back(Request::Call(request)),
                }
            }
        }
    }
}

/// Answer everything still outstanding as abandoned, in the order it arrived.
async fn drain_as_abandoned(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Request>,
    pending: &mut std::collections::VecDeque<Request>,
) -> Vec<Value> {
    let mut responses = Vec::new();
    loop {
        let item = match pending.pop_front() {
            Some(item) => Some(item),
            None => rx.recv().await,
        };
        let Some(item) = item else { break };
        let id = match item {
            Request::Call(request) => request.id,
            Request::Malformed { id, .. } => id,
        };
        responses.push(protocol::err(id, cancel_message(CancelReason::Shutdown)));
    }
    responses
}



async fn write_line<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> anyhow::Result<()> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Last line of defence for the tool surface: a name that is on neither surface is
/// refused with the same wording as an unknown method, so the host cannot tell an
/// unadvertised native tool from a name that never existed.
///
/// The window2 names are admitted here because they are served by a different
/// dispatcher (`crate::x11::window2`); `call` routes them before this guard runs.
fn surface_guard(id: &Value, name: &str) -> Option<Value> {
    if name.is_empty() {
        return Some(protocol::err(
            id.clone(),
            "call requires a tool name",
        ));
    }
    if !is_surface_tool(name) {
        return Some(protocol::err(
            id.clone(),
            format!("{}{}", protocol::UNSUPPORTED_METHOD_PREFIX, name),
        ));
    }
    None
}

/// Whether a `call` name is served by one of the two surfaces.
fn is_surface_tool(name: &str) -> bool {
    SURFACE_TOOLS.contains(&name) || is_window2_method(name)
}

/// Whether a name belongs to the window2 face: the official thirteen plus the DSH extensions
/// that ride the same dispatcher.
///
/// `wait_for` is deliberately kept out of `WINDOW2_TOOLS`, which is the official thirteen and
/// is pinned exactly by a parity test. It still belongs on this face -- it takes a window2
/// window object and is served by the same native backend -- so the surface predicate is the
/// one place that knows the difference between "official" and "served here".
fn is_window2_method(name: &str) -> bool {
    crate::x11::window2::WINDOW2_TOOLS.contains(&name) || name == crate::x11::waitfor::WAIT_FOR_TOOL
}

/// Whether this name has to be sent to the native window2 dispatcher.
///
/// The eight window2-only names always go native: the P1 surface does not define them,
/// and the crate's MCP router has no window-relative coordinate handling.
///
/// The five names both surfaces carry (`click`, `press_key`, `type_text`, `scroll`,
/// `drag`) are decided by the call's own `surface` tag. `window2`/`computer`/`windows`
/// mean the turn is on a window2 face, where those calls arrive in the window2 parameter
/// shape (a window object plus an element_index), so they must reach the window2 handler;
/// `sky.window`/`linux` and an absent tag mean a P1 caller, whose parameter shape is the
/// crate's own, so the name keeps its P1 handler. That preserves the P1 contract exactly
/// while letting a window2 turn address a real window. See `src/sidecar.js`, which is
/// what tags the request.
fn is_window2_native_call(name: &str, call_surface: Option<&str>) -> bool {
    if !is_window2_method(name) {
        return false;
    }
    if !SURFACE_TOOLS.contains(&name) {
        // A window2-only name -- and a DSH extension like wait_for -- has no P1 handler to
        // fall back to, so it goes native whatever surface tag the call carries.
        return true;
    }
    // A shared name follows the surface the caller declared for this call. The tag is
    // normalised here so this predicate is correct on its own, whatever case the host uses.
    let surface = call_surface.map(str::to_ascii_lowercase);
    surface.as_deref().is_some_and(is_window2_surface)
}

/// Which surface a `tools`/`health` request is asking about.
///
/// The JS sidecar sends `surface` as an ordinary request parameter
/// (`src/sidecar.js`, `listTools`), so the helper reads it rather than inventing a
/// second protocol field. `window2` and `computer` are the same surface under the two
/// names the host uses.
fn requested_surface(params: &Map<String, Value>) -> String {
    protocol::json_str(params, "surface")
        .unwrap_or_else(|| "sky.window".to_string())
        .to_ascii_lowercase()
}

fn is_window2_surface(surface: &str) -> bool {
    matches!(surface, "window2" | "computer" | "windows" | "all")
}

/// The sidecar verbs, plus the always-available health/tools/prompt reads.
async fn dispatch_verb(
    service: &ComputerUseLinux,
    state: &Arc<HelperState>,
    method: &str,
    params: &Map<String, Value>,
) -> (Result<Value, String>, bool) {
    match method {
        "health" => (Ok(health_payload(service, state, params).await), false),
        "tools" => (Ok(tools_payload(service, params)), false),
        "prompt" => (
            Ok(json!({
                "prompt": API_PROMPT,
                "surface": "sky.window",
                "source": "helper-rs/assets/prompts/api.md",
            })),
            false,
        ),
        "interrupt" => {
            // The overlay goes away with the call it was shown for; the lease is left
            // for the next observation. helper-rs does this from its own interrupt path.
            if let Some(experience) = experience() {
                experience.interrupt();
            }
            state.interrupted.store(true, Ordering::SeqCst);
            // The worker races this verb against the call in progress and cancels it, so
            // nothing more is needed here beyond the latch that refuses later calls.
            (Ok(json!({"stopped": true})), false)
        }
        "end_turn" => {
            // The JS host reuses one helper process across turns and sends end_turn when
            // the turn changes, so this must NOT strand the helper: it clears the
            // interrupt latch, reports the turn it ended, and is safe to call twice.
            state.interrupted.store(false, Ordering::SeqCst);
            // "hide the overlay, flush the observation lease and re-arm for the next
            // turn" -- the sidecar sends end_turn exactly when the turn scope changes.
            if let Some(experience) = experience() {
                experience.end_turn();
            }
            let (session_id, turn_id) = (
                protocol::json_str(params, "session_id").unwrap_or_default(),
                protocol::json_str(params, "turn_id").unwrap_or_default(),
            );
            (
                Ok(json!({
                    "ended": true,
                    "session_id": session_id,
                    "turn_id": turn_id,
                })),
                false,
            )
        }
        "shutdown" => {
            // A helper that exits while holding a root Escape grab would eat the
            // operator's own Escape key for the rest of the session.
            if let Some(experience) = experience() {
                experience.shutdown();
            }
            state.shutdown.store(true, Ordering::SeqCst);
            state.interrupted.store(true, Ordering::SeqCst);
            (Ok(json!({"closed": true})), true)
        }
        other => (
            Err(format!("{}{}", protocol::UNSUPPORTED_METHOD_PREFIX, other)),
            false,
        ),
    }
}

/// tools: the seven exposed tools with the crate's own JSON Schemas.
fn tools_payload(service: &ComputerUseLinux, params: &Map<String, Value>) -> Value {
    let surface = requested_surface(params);
    if is_window2_surface(&surface) {
        // The window2 surface is served by the native X11 backend, so its schemas come
        // from there rather than from the crate's own MCP table.
        // Exactly the official thirteen, and deliberately not one method more. The DSH
        // `wait_for` extension is dispatchable on this backend but is advertised from the JS
        // tool plane instead of here, because this list is the official contract and is pinned
        // to exactly thirteen by three independent gates (`window2.rs`'s own parity test, the
        // Python `test_computer_surface_is_exactly_the_official_thirteen`, and the headless
        // `run-window2-e2e.sh` driver). Adding a name here would break the parity claim rather
        // than extend it; `dshExtensions` records what else this backend serves.
        return json!({
            "tools": crate::x11::window2::tool_definitions(),
            "surface": if surface == "all" { "all" } else { "window2" },
            "dshExtensions": [crate::x11::waitfor::WAIT_FOR_TOOL],
            "hidden": {
                "note": "Linux window2 exposes the official 13 methods on the native X11 backend; the DSH wait_for extension and the seven sky.window tools are served alongside them.",
                "skyWindowTools": SURFACE_TOOLS,
            },
        });
    }
    let definitions = service.tool_definitions();
    let tools: Vec<Value> = SURFACE_TOOLS
        .iter()
        .filter_map(|name| definitions.iter().find(|tool| tool.name.as_ref() == *name))
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description.clone().unwrap_or_default(),
                "parameters": tool.input_schema,
            })
        })
        .collect();
    json!({
        "tools": tools,
        "surface": "sky.window",
        "hidden": {
            "note": "Linux Computer Use exposes only the sky.window surface through this helper.",
            "nativeTools": definitions.len(),
        },
    })
}

/// health: what this helper is, what it can do here, and what is degraded.
///
/// Nothing is inferred optimistically: every capability comes from the crate's own
/// doctor_report probes, and failed checks are surfaced verbatim in degraded so a
/// blocked screenshot (for example a missing desktop portal backend) is reported
/// honestly instead of being claimed as working.
async fn health_payload(
    service: &ComputerUseLinux,
    state: &Arc<HelperState>,
    params: &Map<String, Value>,
) -> Value {
    let diagnostics = match tokio::task::spawn_blocking(crate::diagnostics::doctor_report).await {
        Ok(report) => Some(report),
        Err(_) => None,
    };

    let mut degraded = Vec::new();
    if let Some(report) = diagnostics.as_ref() {
        if let Ok(value) = serde_json::to_value(report) {
            collect_failed_checks("", &value, &mut degraded);
        }
    }

    let (
        mut capabilities,
        mut readiness,
        mut platform,
        mut portals,
        mut accessibility,
        mut windowing,
        mut input,
    ) = (
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
    );
    if let Some(report) = diagnostics.as_ref() {
        capabilities = serde_json::to_value(&report.capabilities).unwrap_or(Value::Null);
        readiness = serde_json::to_value(&report.readiness).unwrap_or(Value::Null);
        platform = serde_json::to_value(&report.platform).unwrap_or(Value::Null);
        portals = serde_json::to_value(&report.portals).unwrap_or(Value::Null);
        accessibility = serde_json::to_value(&report.accessibility).unwrap_or(Value::Null);
        windowing = serde_json::to_value(&report.windowing).unwrap_or(Value::Null);
        input = serde_json::to_value(&report.input).unwrap_or(Value::Null);
    }

    let surface = requested_surface(params);
    let window2 = window2_health().await;
    json!({
        "ok": true,
        "helper": "dsh-computer-use",
        "helperFlavor": "linux-jsonl",
        "surface": if is_window2_surface(&surface) { "window2" } else { "sky.window" },
        "surfaces": ["sky.window", "window2"],
        "window2": window2,
        "protocol": "stdio-jsonl",
        "methods": HELPER_METHODS,
        "tools": SURFACE_TOOLS,
        "nativeToolsAvailable": service.tool_definitions().len(),
        "version": env!("CARGO_PKG_VERSION"),
        "upstream": {
            "repo": UPSTREAM_REPO,
            "commit": UPSTREAM_COMMIT,
            "crate": "computer-use-linux",
            "crateVersion": UPSTREAM_CRATE_VERSION,
        },
        "platform": platform,
        "capabilities": capabilities,
        "readiness": readiness,
        "portals": portals,
        "accessibility": accessibility,
        "windowing": windowing,
        "input": input,
        "degraded": degraded,
        "interrupted": state.is_interrupted(),
        // Exactly one field, and only for the sessions that have the layer: a second
        // one would grow the health contract the integration line depends on.
        "experience": crate::x11_experience::health_summary(),
    })
}

/// What the native X11 window2 backend can actually do here.
///
/// Every capability is probed, never assumed. The X11 backend is reported as degraded
/// (with the reason and the method names it affects) rather than silently absent, and
/// "composite capture" is listed separately from plain capture because they do not
/// guarantee the same thing: only the composite path is occlusion-proof, and on a
/// session with a running compositor it may be unavailable altogether.
async fn window2_health() -> Value {
    let connected = tokio::task::spawn_blocking(crate::x11::connection::capabilities).await;
    let capabilities = match connected {
        Ok(Ok(capabilities)) => capabilities,
        Ok(Err(error)) => {
            return json!({
                "available": false,
                "backend": crate::x11::X11_NATIVE_BACKEND,
                "methods": [],
                "degraded": [format!("the X11 window2 surface is unavailable: {error}")],
            });
        }
        Err(error) => {
            return json!({
                "available": false,
                "backend": crate::x11::X11_NATIVE_BACKEND,
                "methods": [],
                "degraded": [format!("the X11 capability probe did not finish: {error}")],
            });
        }
    };

    let mut degraded = capabilities.detail.clone();
    let mut methods: Vec<&str> = Vec::new();
    // Every window2 method this backend could not serve would be listed here. Nothing is
    // refused on X11 at the moment: launch_app used to be the one entry, and it is now
    // served through the desktop entries and $PATH.
    let refused: Vec<Value> = Vec::new();

    methods.push("list_windows");
    methods.push("get_window");
    methods.push("list_apps");
    // launch_app resolves through the XDG desktop entries and $PATH, so it is served here
    // rather than refused; the resolution is what the Windows helper gets from the shell's
    // application registry.
    methods.push("launch_app");
    methods.push("activate_window");
    if capabilities.xtest.is_some() {
        methods.extend(["click", "press_key", "type_text", "scroll", "drag"]);
    } else {
        degraded.push(
            "XTest is missing, so click/press_key/type_text/scroll/drag cannot inject input"
                .to_string(),
        );
    }
    methods.push("get_window_state");
    // DSH extension on the same surface/dispatcher; listed here so health describes what this
    // backend actually serves, with `refused` staying empty because nothing is refused.
    methods.push(crate::x11::waitfor::WAIT_FOR_TOOL);
    if capabilities.composite.is_none() {
        degraded.push(
            "XComposite is missing, so get_window_state falls back to a direct window read and              occluding windows may appear in the screenshot"
                .to_string(),
        );
    }
    if capabilities.window_manager.is_none() {
        degraded.push(
            "no EWMH window manager is running (no _NET_SUPPORTING_WM_CHECK), so windows are              enumerated from the root tree and raise/focus requests fall back to map + restack"
                .to_string(),
        );
    }
    if capabilities.shm.is_none() {
        degraded.push(
            "MIT-SHM is missing, so captures go through a slower synchronous GetImage".to_string(),
        );
    }

    json!({
        "available": true,
        "backend": crate::x11::X11_NATIVE_BACKEND,
        "screen": { "width": capabilities_screen().0, "height": capabilities_screen().1 },
        "extensions": {
            "shm": capabilities.shm.map(|(major, minor)| format!("{major}.{minor}")),
            "xtest": capabilities.xtest.map(|(major, minor)| format!("{major}.{minor}")),
            "xfixes": capabilities.xfixes.map(|(major, minor)| format!("{major}.{minor}")),
            "composite": capabilities.composite.map(|(major, minor)| format!("{major}.{minor}")),
        },
        "windowManager": capabilities.window_manager.map(|window| format!("0x{window:x}")),
        "methods": methods,
        "refused": refused,
        "degraded": degraded,
        // Stated plainly because it is the one place this backend cannot match the
        // Windows helper: DWM keeps a full backing bitmap per window, plain X11 does
        // not, so a fully obscured window that has not repainted yields its last
        // painted content rather than live pixels.
        // The continuation backslashes strip the newline and the next line's leading
        // whitespace, so the sentence reads as one line wherever it is displayed.
        "occlusionNote": "composite capture returns the window's own pixels; a window that is \
                          fully obscured and has not repainted yields its last painted \
                          content, which is weaker than the Windows DWM guarantee",
    })
}

fn capabilities_screen() -> (u16, u16) {
    crate::x11::connection::with_connection(|connection| connection.screen_size())
        .unwrap_or((0, 0))
}

/// Walk a serialized report and name every failed check as path: detail.
fn collect_failed_checks(path: &str, node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            let failed = map.get("ok").and_then(Value::as_bool) == Some(false);
            if failed {
                if let Some(detail) = map.get("detail").and_then(Value::as_str) {
                    let label = if path.is_empty() { "check" } else { path };
                    out.push(format!("{label}: {detail}"));
                }
            }
            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                collect_failed_checks(&child_path, child, out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_failed_checks(&format!("{path}[{index}]"), child, out);
            }
        }
        _ => {}
    }
}

/// Flush stdout the way the JSONL host expects before the process exits.
pub fn flush_stdout() {
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the real stream: feed lines in, collect the lines that come back.
    async fn run_stream(lines: &str) -> (String, String) {
        let (mut client, server) = tokio::io::duplex(1 << 20);
        let (out_writer, out_reader) = tokio::io::duplex(1 << 20);
        let serving = tokio::spawn(serve_stream(server, out_writer));
        {
            use tokio::io::AsyncWriteExt;
            let payload = lines.as_bytes().to_vec();
            let _ = client.write_all(&payload).await;
            let _ = client.flush().await;
        }
        drop(client);

        use tokio::io::AsyncReadExt;
        let mut reader = tokio::io::BufReader::new(out_reader);
        let mut collected = String::new();
        let _ = reader.read_to_string(&mut collected).await;
        let error = match serving.await {
            Ok(result) => result.err().map(|e| e.to_string()).unwrap_or_default(),
            Err(e) => e.to_string(),
        };
        (collected, error)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_stream_answers_every_line_in_order() {
        let (out, error) = run_stream(concat!(
            "{\"id\":1,\"method\":\"health\",\"params\":{},\"meta\":{}}\n",
            "{\"id\":2,\"method\":\"tools\",\"params\":{},\"meta\":{}}\n",
            "{\"id\":3,\"method\":\"nope\",\"params\":{},\"meta\":{}}\n",
            "{\"id\":4,\"method\":\"call\",\"params\":{\"name\":\"move_window\"},\"meta\":{}}\n",
            "{\"id\":5,\"method\":\"shutdown\",\"params\":{},\"meta\":{}}\n",
        ))
        .await;
        assert_eq!(error, "");
        let ids: Vec<i64> = out
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["id"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5], "responses must stay in request order");
        assert!(out.contains("unsupported method: nope"), "out was: {out}");
        assert!(out.contains("\"closed\":true"), "out was: {out}");
        // An unadvertised native tool is refused on its own merits. It must NOT be
        // retroactively cancelled by a shutdown the host sent afterwards: a request is
        // judged against the state at its own point in the stream.
        //
        // move_window is the example because it is a real crate tool that no surface
        // advertises: neither one of the seven sky.window tools nor one of the thirteen
        // window2 methods. (drag used to serve here, but window2 now exposes it.)
        let fourth = out
            .lines()
            .find(|line| line.contains("\"id\":4"))
            .expect("id 4 must be answered");
        assert!(
            fourth.contains("unsupported method: move_window"),
            "id 4 was answered with {fourth}"
        );
        assert!(!fourth.contains("shutting down"), "shutdown reached back in time");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interrupt_cancels_a_call_that_is_still_running() {
        // A screenshot on a real desktop takes long enough to be interrupted. Whatever
        // the backend does here, the call must not report success after an interrupt.
        let (out, error) = run_stream(concat!(
            "{\"id\":1,\"method\":\"call\",\"params\":{\"name\":\"screenshot\",\"arguments\":{}},\"meta\":{\"x-oai-cua-request-budget-ms\":60000}}\n",
            "{\"id\":2,\"method\":\"interrupt\",\"params\":{},\"meta\":{}}\n",
            "{\"id\":3,\"method\":\"end_turn\",\"params\":{},\"meta\":{}}\n",
            "{\"id\":4,\"method\":\"shutdown\",\"params\":{},\"meta\":{}}\n",
        ))
        .await;
        assert_eq!(error, "");
        let by_id: std::collections::HashMap<i64, Value> = out
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let value: Value = serde_json::from_str(line).unwrap();
                (value["id"].as_i64().unwrap(), value)
            })
            .collect();
        assert_eq!(by_id.len(), 4, "interrupt, end_turn and shutdown must all answer");
        // The stopped call must be reported as stopped, never as a success.
        if by_id[&1]["ok"] == json!(true) {
            assert!(
                by_id[&1]["result"]["value"].is_object() || by_id[&1]["result"]["value"].is_null(),
                "a completed screenshot still has to carry a normal call result"
            );
        } else {
            let message = by_id[&1]["error"].as_str().unwrap_or_default();
            assert!(
                message.contains("stopped") || message.contains("interrupt") || message.contains("budget"),
                "stopped call message was {message:?}"
            );
        }
        assert_eq!(by_id[&2]["result"]["stopped"], json!(true));
        assert_eq!(by_id[&3]["result"]["ended"], json!(true));
        assert_eq!(by_id[&4]["result"]["closed"], json!(true));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn requests_after_a_call_are_answered_after_it() {
        // Two real calls in a row: the second must not start before the first is answered,
        // and both must be answered under their own ids.
        let (out, error) = run_stream(concat!(
            "{\"id\":1,\"method\":\"call\",\"params\":{\"name\":\"list_apps\",\"arguments\":{}},\"meta\":{\"x-oai-cua-request-budget-ms\":30000}}\n",
            "{\"id\":2,\"method\":\"call\",\"params\":{\"name\":\"get_app_state\",\"arguments\":{\"include_screenshot\":false}},\"meta\":{\"x-oai-cua-request-budget-ms\":30000}}\n",
            "{\"id\":3,\"method\":\"shutdown\",\"params\":{},\"meta\":{}}\n",
        ))
        .await;
        assert_eq!(error, "");
        let ids: Vec<i64> = out
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
        for line in out.lines().filter(|line| line.contains("\"id\":1") || line.contains("\"id\":2")) {
            let message: Value = serde_json::from_str(line).unwrap();
            if message["ok"] == json!(true) {
                let result = &message["result"];
                assert_eq!(result["ok"], json!(true));
                assert!(result["name"].is_string());
                assert!(result["value"].is_object(), "a successful call carries a value");
                assert!(result["images"].is_array());
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn nothing_runs_after_shutdown_but_nothing_is_left_hanging() {
        // shutdown is terminal: a request that arrived after it is never dispatched. It is
        // still *answered*, because a silent drop would leave the host waiting out its full
        // timeout for a response that can never come.
        let (out, error) = run_stream(concat!(
            "{\"id\":1,\"method\":\"shutdown\",\"params\":{},\"meta\":{}}\n",
            "{\"id\":2,\"method\":\"call\",\"params\":{\"name\":\"list_apps\",\"arguments\":{}},\"meta\":{}}\n",
        ))
        .await;
        assert_eq!(error, "");
        let responses: Vec<(i64, Value)> = out
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let value: Value = serde_json::from_str(line).unwrap();
                (value["id"].as_i64().unwrap(), value)
            })
            .collect();
        assert_eq!(
            responses.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![1, 2],
            "responses must stay in request order"
        );
        assert_eq!(responses[0].1["result"]["closed"], json!(true));
        // The post-shutdown call was abandoned, not executed.
        assert_eq!(responses[1].1["ok"], json!(false));
        assert!(responses[1].1["error"]
            .as_str()
            .unwrap_or_default()
            .contains("shutting down"));
        // A late request MUST not have produced a call result.
        assert!(responses[1].1.get("result").is_none());
    }

    #[test]
    fn surface_is_exactly_the_seven_sky_window_tools() {
        assert_eq!(
            SURFACE_TOOLS,
            &[
                "list_apps",
                "get_app_state",
                "screenshot",
                "click",
                "scroll",
                "press_key",
                "type_text"
            ]
        );
    }

    #[test]
    fn sidecar_verbs_stay_reachable_while_interrupted() {
        for method in ["health", "tools", "prompt", "interrupt", "shutdown", "end_turn"] {
            assert!(is_lifecycle_method(method), "{method} must stay reachable");
        }
        assert!(!is_lifecycle_method("call"));
    }

    #[test]
    fn failed_checks_are_named_with_their_path() {
        let report = json!({
            "portals": {"screencast": {"ok": false, "detail": "no portal backend"}},
            "accessibility": {"at_spi_bus": {"ok": true, "detail": "connected"}},
        });
        let mut out = Vec::new();
        collect_failed_checks("", &report, &mut out);
        assert_eq!(out, vec!["portals.screencast: no portal backend"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn health_reports_capabilities_and_never_invents_success() {
        let service = ComputerUseLinux::default();
        let state = Arc::new(HelperState::default());
        let health = health_payload(&service, &state, &Map::new()).await;
        assert_eq!(health["ok"], json!(true));
        assert_eq!(health["helper"], json!("dsh-computer-use"));
        assert_eq!(health["surface"], json!("sky.window"));
        assert_eq!(health["upstream"]["commit"], json!(UPSTREAM_COMMIT));
        assert_eq!(health["tools"].as_array().unwrap().len(), 7);
        // A capability report is always present, even when everything is degraded.
        assert!(health["readiness"].is_object());
        assert!(health["degraded"].is_array());
        assert!(!state.is_interrupted());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_lists_only_the_seven_with_real_schemas() {
        let service = ComputerUseLinux::default();
        let payload = tools_payload(&service, &Map::new());
        let tools = payload["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert_eq!(names, SURFACE_TOOLS);
        for tool in tools {
            assert!(
                tool["parameters"].is_object(),
                "{} must carry a JSON schema",
                tool["name"]
            );
            assert!(!tool["description"].as_str().unwrap_or_default().is_empty());
        }
        // The catalog the host sees must not leak the other native tools.
        assert!(names.iter().all(|name| SURFACE_TOOLS.contains(name)));
    }

    #[test]
    fn an_unadvertised_native_tool_is_refused_like_an_unknown_method() {
        // The guard is what makes the surface closed: neither a native tool that exists in
        // this crate nor a name that never existed may reach a handler.
        //
        // move_window stays the example because it is a real crate tool advertised by
        // neither surface; drag is no longer usable as one since window2 exposes it.
        let response = surface_guard(&json!(5), "move_window").expect("move_window must be refused");
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"], json!("unsupported method: move_window"));
        assert_eq!(response["id"], json!(5));

        let unknown = surface_guard(&json!(6), "nope_not_a_tool").expect("unknown must be refused");
        assert_eq!(unknown["error"], json!("unsupported method: nope_not_a_tool"));

        let nameless = surface_guard(&json!(7), "").expect("a missing name must be refused");
        assert_eq!(nameless["error"], json!("call requires a tool name"));

        for allowed in SURFACE_TOOLS {
            assert!(surface_guard(&json!(1), allowed).is_none(), "{allowed} must pass the guard");
        }
        // Every window2 method must also pass the guard: it is a second surface, not a
        // hole in the first one.
        for allowed in crate::x11::window2::WINDOW2_TOOLS {
            assert!(
                surface_guard(&json!(1), allowed).is_none(),
                "{allowed} must pass the guard"
            );
        }
    }

    #[test]
    fn a_name_on_both_surfaces_follows_the_call_surface() {
        // Without a surface tag a shared name has to resolve the way it did before
        // window2 existed, and a P1 tag must keep saying the same thing. Only a window2
        // tag reroutes the five shared names to the native dispatcher.
        let shared: Vec<&str> = crate::x11::window2::WINDOW2_TOOLS
            .iter()
            .copied()
            .filter(|name| SURFACE_TOOLS.contains(name))
            .collect();
        assert_eq!(shared.len(), 5, "expected the five shared names");
        for name in &shared {
            assert!(
                !is_window2_native_call(name, None),
                "{name} must keep the sky.window handler when the call is untagged"
            );
            for p1 in ["sky.window", "linux"] {
                assert!(
                    !is_window2_native_call(name, Some(p1)),
                    "{name} must keep the sky.window handler on surface={p1}"
                );
            }
            for window2 in ["window2", "computer", "windows", "all", "COMPUTER"] {
                assert!(
                    is_window2_native_call(name, Some(window2)),
                    "{name} must go to the window2 dispatcher on surface={window2}"
                );
            }
        }
        for name in crate::x11::window2::WINDOW2_TOOLS {
            if !SURFACE_TOOLS.contains(name) {
                // A window2-only name has no P1 handler, so it goes native on every
                // surface, including an absent tag.
                for surface in [None, Some("sky.window"), Some("linux"), Some("computer")] {
                    assert!(
                        is_window2_native_call(name, surface),
                        "{name} is window2-only and must be routed natively"
                    );
                }
            }
        }
    }

    #[test]
    fn a_window2_tag_reroutes_only_the_methods_window2_can_serve() {
        // The tag must not become a hole in the P1 guard: a name neither surface defines
        // is still refused, and a `call` that carries a surface it cannot serve keeps the
        // guard's own wording.
        assert!(surface_guard(&json!(1), "get_app_state").is_none());
        assert!(!is_window2_native_call("get_app_state", Some("computer")));
        assert!(!is_window2_native_call("screenshot", Some("computer")));
        assert!(!is_window2_native_call("move_window", Some("computer")));
        assert_eq!(
            surface_guard(&json!(2), "move_window").expect("refused")["error"],
            json!("unsupported method: move_window")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_refused_tool_never_reaches_a_handler() {
        // End to end through the worker: an unadvertised tool is answered, not executed.
        let (out, error) = run_stream(concat!(
            "{\"id\":1,\"method\":\"call\",\"params\":{\"name\":\"move_window\",\"arguments\":{}},\"meta\":{}}\n",
            "{\"id\":2,\"method\":\"shutdown\",\"params\":{},\"meta\":{}}\n",
        ))
        .await;
        assert_eq!(error, "");
        assert!(out.contains("unsupported method: move_window"), "out was: {out}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn end_turn_clears_an_interrupt_and_is_idempotent() {
        let service = ComputerUseLinux::default();
        let state = Arc::new(HelperState::default());
        let (_result, _stop) = dispatch_verb(&service, &state, "interrupt", &Map::new()).await;
        assert!(state.is_interrupted());
        let (result, stop) = dispatch_verb(&service, &state, "end_turn", &Map::new()).await;
        assert!(!state.is_interrupted());
        assert_eq!(result.unwrap()["ended"], json!(true));
        assert!(!stop);
        let (again, _stop) = dispatch_verb(&service, &state, "end_turn", &Map::new()).await;
        assert_eq!(again.unwrap()["ended"], json!(true));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_asks_the_loop_to_stop() {
        let service = ComputerUseLinux::default();
        let state = Arc::new(HelperState::default());
        let (result, stop) = dispatch_verb(&service, &state, "shutdown", &Map::new()).await;
        assert_eq!(result.unwrap()["closed"], json!(true));
        assert!(stop);
        assert!(state.is_shutdown());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_methods_are_refused_with_the_official_wording() {
        let service = ComputerUseLinux::default();
        let state = Arc::new(HelperState::default());
        let (result, _stop) = dispatch_verb(&service, &state, "launch_app", &Map::new()).await;
        assert_eq!(result.unwrap_err(), "unsupported method: launch_app");
    }
}