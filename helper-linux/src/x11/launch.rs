//! Application resolution and detached launching for the X11 window2 surface.
//!
//! The Windows helper resolves an app id through the shell's application registry and
//! then waits for the app to expose a window. X11 has no registry, but it has something
//! equivalent and standardised: the freedesktop desktop entry. This module reads those
//! entries, falls back to PATH, spawns the program detached from the helper, and then
//! waits for the window manager to publish a window that belongs to it.
//!
//! Two properties are load-bearing, and both are why this is not a one-line spawn:
//!
//! * The helper must never become the parent of the launched app. The helper is a stdio
//!   JSONL sidecar that exits with its session; if the app were its child, quitting the
//!   helper would either reap a live app or leave a zombie. The spawn therefore goes
//!   through setsid plus an intermediate fork that exits immediately, so the app is
//!   reparented to init and outlives the helper. PR_SET_PDEATHSIG is deliberately NOT
//!   set: it would kill the app whenever the helper exited, which is the exact opposite of
//!   what a launcher must do.
//! * A second launch of a running app must not happen. Many applications treat a second
//!   invocation as "new instance" and produce a second window, a duplicate tray icon, or a
//!   second login prompt. Deduplication is the core motivation for this module, so every
//!   launch first looks for an existing window that belongs to the requested app and
//!   raises that instead of spawning anything.
//!
//! Nothing here guesses. An app that cannot be resolved is refused with a structured,
//! model-readable error rather than by spawning an arbitrary string as a program, which
//! would be an injection hole rather than a feature.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use super::window::{self, X11Window};

/// How long a launch waits for a window by default.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// The environment variable that overrides the window wait.
pub const TIMEOUT_ENV: &str = "DSH_CUA_LAUNCH_TIMEOUT_MS";

/// Bounds on the configurable wait.
///
/// A wait shorter than half a second cannot cover even a warm app that has not mapped its
/// window yet, and a wait longer than a minute turns a hung launcher into a helper that
/// looks dead to the host (the helper is stdio-serial, so a long call delays health and
/// interrupt behind it). Values outside the range are clamped rather than rejected: the
/// variable is an operator convenience, not a contract to enforce.
const MIN_TIMEOUT_MS: u64 = 500;
const MAX_TIMEOUT_MS: u64 = 60_000;

/// How often the wait re-reads the client list.
///
/// 100 ms costs 100 client-list reads over the default wait, which is cheap next to the
/// round trip of a window that appears a whole second late.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Why a launch did not happen, in the two shapes the caller has to distinguish.
#[derive(Debug)]
pub enum LaunchFailure {
    /// The app could not be resolved to a program. This is a structured refusal, not an
    /// error: the model can read the reason and change approach.
    Refused { reason: String, alternative: String },
    /// Resolution succeeded but the launch itself failed (no X session, spawn failure).
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for LaunchFailure {
    fn from(error: anyhow::Error) -> Self {
        LaunchFailure::Failed(error)
    }
}

/// One desktop entry, reduced to what launching needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// The file stem, which is the desktop-file id (qq for qq.desktop).
    pub id: String,
    pub path: PathBuf,
    /// The untranslated Name.
    pub name: String,
    /// The raw Exec line, field codes included.
    pub exec: String,
    pub startup_wm_class: Option<String>,
    pub terminal: bool,
}

/// A program ready to be spawned, plus the keys that identify its windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedApp {
    /// What the caller asked for, trimmed.
    pub requested: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The window-matching keys, already normalized.
    pub keys: Vec<String>,
    /// Where the resolution came from, for the response and for diagnostics.
    pub source: String,
    pub desktop_name: Option<String>,
}

impl ResolvedApp {
    /// The resolution as the response reports it.
    pub fn describe(&self) -> Value {
        json!({
            "requested": self.requested,
            "name": self.desktop_name,
            "program": self.program.display().to_string(),
            "args": self.args,
            "source": self.source,
        })
    }
}

/// The outcome of a launch request.
pub struct LaunchOutcome {
    /// False when an already-running instance was raised instead of starting one.
    pub launched: bool,
    pub already_running: bool,
    pub window: Option<X11Window>,
    /// An honest caveat, present when the outcome is not fully determined.
    pub note: Option<String>,
    pub resolved: ResolvedApp,
}

/// Normalize a name for window matching.
///
/// WM_CLASS is matched case-insensitively by every desktop shell, and the characters the
/// two sides disagree about are exactly the separators: a desktop entry named
/// "Clash Verge" belongs to the window whose class is "Clash-verge". Only alphanumerics
/// survive, so both become "clashverge".
pub fn normalize(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The wait for a window, from the environment when it is usable.
pub fn window_wait() -> Duration {
    let configured = std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS))
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    Duration::from_millis(configured)
}

/// Split an Exec line into a program and its arguments, dropping field codes.
///
/// The desktop entry specification gives Exec a small shell-like grammar with no shell:
/// single and double quotes group a token, a backslash escapes the next character, %% means
/// a literal percent sign, and the other % codes ask for files or URLs the caller does not
/// have. Those codes are removed rather than expanded because there is nothing to expand
/// them with, and because passing a literal %U to an app is worse than passing nothing: the
/// app would look for a file named %U and start with a broken document.
pub fn parse_exec(exec: &str) -> Result<(String, Vec<String>)> {
    let tokens = tokenize_exec(exec);
    let mut program: Option<String> = None;
    let mut args = Vec::new();
    for token in tokens {
        // %% is a literal percent, which has to be undone before the token can be tested
        // for being a field code: a lone %% is a literal percent argument.
        let token = token.replace("%%", "%");
        if is_field_code(&token) {
            continue;
        }
        match program {
            None => program = Some(token),
            Some(_) => args.push(token),
        }
    }
    let program = program
        .filter(|program| !program.is_empty())
        .ok_or_else(|| anyhow!("the Exec line {exec:?} names no program"))?;
    Ok((program, args))
}

/// The % codes the specification defines.
fn is_field_code(token: &str) -> bool {
    let mut characters = token.chars();
    let (Some('%'), Some(code), None) = (characters.next(), characters.next(), characters.next())
    else {
        return false;
    };
    matches!(
        code,
        'f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'i' | 'c' | 'k' | 'v' | 'm'
    )
}

/// The tokenizer behind parse_exec, kept separate so it can be tested on its own.
fn tokenize_exec(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                if let Some(escaped) = characters.next() {
                    current.push(escaped);
                    started = true;
                }
            }
            '"' => {
                started = true;
                while let Some(inner) = characters.next() {
                    match inner {
                        '"' => break,
                        '\\' => {
                            if let Some(escaped) = characters.next() {
                                current.push(escaped);
                            }
                        }
                        _ => current.push(inner),
                    }
                }
            }
            '\'' => {
                started = true;
                for inner in characters.by_ref() {
                    if inner == '\'' {
                        break;
                    }
                    current.push(inner);
                }
            }
            other if other.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            other => {
                current.push(other);
                started = true;
            }
        }
    }
    if started {
        tokens.push(current);
    }
    tokens
}

/// Parse the [Desktop Entry] group of a desktop file.
///
/// Returns None when the entry is not one that should be launched: a link, a hidden or
/// suppressed entry, or one whose TryExec names a program this machine does not have. Those
/// are skipped rather than launched because a desktop shell would not show them either, so
/// a caller asking for that exact name is better served by a refusal that says the app is
/// not installed than by a spawn that fails silently.
pub fn parse_desktop_entry(id: &str, path: &Path, content: &str) -> Option<DesktopEntry> {
    let mut in_entry = false;
    let mut kind: Option<String> = None;
    let mut name: Option<String> = None;
    let mut exec: Option<String> = None;
    let mut startup_wm_class: Option<String> = None;
    let mut terminal = false;
    let mut hidden = false;
    let mut no_display = false;
    let mut try_exec: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        // Localized keys (Name[de]) are skipped: the untranslated key is the one the model
        // is expected to match, and picking a locale the operator did not ask for would
        // make resolution depend on the environment in a way nobody can predict.
        if key.contains('[') {
            continue;
        }
        match key {
            "Type" => kind = Some(value.to_string()),
            "Name" => name = Some(value.to_string()),
            "Exec" => exec = Some(value.to_string()),
            "StartupWMClass" => startup_wm_class = Some(value.to_string()),
            "TryExec" => try_exec = Some(value.to_string()),
            "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }

    if hidden || no_display {
        return None;
    }
    if kind.as_deref().is_some_and(|kind| kind != "Application") {
        return None;
    }
    if let Some(try_exec) = try_exec.as_deref() {
        if !try_exec.is_empty() && program_in_path(try_exec, None).is_none() {
            return None;
        }
    }
    let exec = exec.filter(|exec| !exec.trim().is_empty())?;
    Some(DesktopEntry {
        id: id.to_string(),
        path: path.to_path_buf(),
        name: name.unwrap_or_else(|| id.to_string()),
        exec,
        startup_wm_class,
        terminal,
    })
}

/// The applications directories to search, most specific first.
///
/// XDG_DATA_HOME (or ~/.local/share) comes before XDG_DATA_DIRS, which is the precedence
/// the specification gives: a user's own entry overrides the system one of the same name,
/// and a test that points XDG_DATA_HOME at a temporary directory therefore fully controls
/// resolution.
pub fn desktop_roots() -> Vec<PathBuf> {
    search_roots_from(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("XDG_DATA_DIRS").ok().as_deref(),
    )
}

/// The pure half of desktop_roots, so the search order can be tested without the
/// environment.
pub fn search_roots_from(data_home: Option<&str>, data_dirs: Option<&str>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push = |base: &str| {
        let base = base.trim();
        if base.is_empty() {
            return;
        }
        let path = Path::new(base).join("applications");
        if !roots.contains(&path) {
            roots.push(path);
        }
    };

    match data_home.map(str::trim).filter(|value| !value.is_empty()) {
        Some(home) => push(home),
        None => {
            if let Some(home) = std::env::var("HOME").ok().filter(|value| !value.is_empty()) {
                push(&Path::new(&home).join(".local/share").display().to_string());
            }
        }
    }
    // The specification's default when XDG_DATA_DIRS is unset.
    let dirs = data_dirs
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for dir in dirs.split(':') {
        push(dir);
    }
    roots
}

/// Find the desktop entry a caller's app name refers to.
///
/// Matching is case-insensitive and runs in two passes across *all* roots before it
/// weakens the criterion, so an exact Name/StartupWMClass/id match anywhere in the search
/// path wins over an Exec basename match anywhere else. Within one pass the roots are in
/// precedence order, which is what keeps the user's own entry ahead of the system one of
/// the same name.
pub fn find_desktop_entry(app: &str) -> Option<DesktopEntry> {
    find_desktop_entry_in(&desktop_roots(), app)
}

/// The pure half of find_desktop_entry.
pub fn find_desktop_entry_in(roots: &[PathBuf], app: &str) -> Option<DesktopEntry> {
    let wanted = app.trim();
    if wanted.is_empty() {
        return None;
    }
    let wanted_normalized = normalize(wanted);
    let entries = read_desktop_entries(roots);

    for entry in &entries {
        for key in desktop_strong_keys(entry) {
            if key.eq_ignore_ascii_case(wanted) || normalized_eq(&key, &wanted_normalized) {
                return Some(entry.clone());
            }
        }
    }
    for entry in &entries {
        let Ok((program, _)) = parse_exec(&entry.exec) else {
            continue;
        };
        let basename = Path::new(&program)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if basename.eq_ignore_ascii_case(wanted) || normalized_eq(&basename, &wanted_normalized) {
            return Some(entry.clone());
        }
    }
    None
}

/// Names a desktop shell would match a window against.
fn desktop_strong_keys(entry: &DesktopEntry) -> Vec<String> {
    let mut keys = vec![entry.id.clone(), entry.name.clone()];
    if let Some(class) = entry.startup_wm_class.as_ref() {
        keys.push(class.clone());
    }
    keys
}

fn normalized_eq(left: &str, right_normalized: &str) -> bool {
    let left = normalize(left);
    // Two characters is the shortest key that is still a name rather than noise; without
    // this floor an app called "qt" would match every entry mentioning qt.
    left.len() >= 2 && left == *right_normalized
}

/// Read and parse every desktop entry under the roots, in precedence order.
fn read_desktop_entries(roots: &[PathBuf]) -> Vec<DesktopEntry> {
    let mut entries = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for root in roots {
        let Ok(listing) = std::fs::read_dir(root) else {
            continue;
        };
        let mut files: Vec<PathBuf> = listing
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|extension| extension == "desktop")
                    && path.is_file()
            })
            .collect();
        files.sort();
        for file in files {
            let Some(id) = file.file_stem().map(|stem| stem.to_string_lossy().to_string()) else {
                continue;
            };
            // A user entry shadows a system entry with the same file name, which is the
            // documented override mechanism; the first one seen wins because the roots are
            // already in precedence order.
            if seen.contains(&id) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&file) else {
                continue;
            };
            if let Some(entry) = parse_desktop_entry(&id, &file, &content) {
                seen.push(id);
                entries.push(entry);
            }
        }
    }
    entries
}

/// Turn a desktop entry into a spawnable program.
fn resolved_from_entry(entry: &DesktopEntry) -> Option<ResolvedApp> {
    let (program_name, args) = parse_exec(&entry.exec).ok()?;
    let program = program_in_path(&program_name, None)?;

    let mut keys = vec![
        entry.name.clone(),
        entry.id.clone(),
        Path::new(&program_name)
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_default(),
    ];
    if let Some(class) = entry.startup_wm_class.as_ref() {
        keys.push(class.clone());
    }

    let (program, args, source) = if entry.terminal {
        // A Terminal=true entry is a console program: starting it directly would run it
        // with no terminal to draw in, so the emulator runs it instead. When no emulator
        // exists the program is started directly and the response says so.
        match program_in_path("x-terminal-emulator", None) {
            Some(emulator) => {
                let mut wrapped = vec![program.display().to_string()];
                wrapped.extend(args.iter().cloned());
                (
                    emulator,
                    std::iter::once("-e".to_string()).chain(wrapped).collect(),
                    format!("desktop:{} (terminal)", entry.path.display()),
                )
            }
            None => (
                program,
                args,
                format!("desktop:{} (no terminal emulator)", entry.path.display()),
            ),
        }
    } else {
        (program, args, format!("desktop:{}", entry.path.display()))
    };

    Some(ResolvedApp {
        requested: entry.name.clone(),
        program,
        args,
        keys: clean_keys(keys),
        source,
        desktop_name: Some(entry.name.clone()),
    })
}

/// Resolve an executable name through PATH, or a path as given.
pub fn program_in_path(name: &str, path_var: Option<&str>) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if name.contains('/') {
        let path = PathBuf::from(name);
        return is_executable(&path).then(|| canonical(&path));
    }
    let path_var = match path_var {
        Some(value) => value.to_string(),
        None => std::env::var("PATH").ok()?,
    };
    for directory in path_var.split(':') {
        // An empty PATH component means the current directory in POSIX, but resolving an
        // app name against the helper's working directory is a surprise, not a feature.
        if directory.is_empty() {
            continue;
        }
        let candidate = Path::new(directory).join(name);
        if is_executable(&candidate) {
            return Some(canonical(&candidate));
        }
    }
    None
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether a path names a file this process could execute.
///
/// Checked before the spawn because the spawn's error reporting cannot cover it: the app is
/// started by a grandchild after two forks, so an exec failure there has no channel back to
/// the helper and would surface as a silent no-op. A missing or non-executable program is
/// therefore turned into a resolution failure, which the caller can report.
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

fn clean_keys(keys: Vec<String>) -> Vec<String> {
    let mut cleaned: Vec<String> = Vec::new();
    for key in keys {
        let key = normalize(&key);
        if key.len() >= 2 && !cleaned.contains(&key) {
            cleaned.push(key);
        }
    }
    cleaned
}

/// Resolve an app name to a program: desktop entry first, then PATH.
pub fn resolve(app: &str) -> Result<ResolvedApp, LaunchFailure> {
    let requested = app.trim();
    if requested.is_empty() {
        return Err(LaunchFailure::Refused {
            reason: "launch_app needs an app name: the app argument was empty".to_string(),
            alternative: "pass the app id from list_apps(), a desktop entry name such as \
                          \"firefox\", or an executable name or path"
                .to_string(),
        });
    }

    let roots = desktop_roots();
    if let Some(entry) = find_desktop_entry_in(&roots, requested) {
        if let Some(resolved) = resolved_from_entry(&entry) {
            return Ok(resolved);
        }
    }

    let path = std::env::var("PATH").ok();
    if let Some(program) = program_in_path(requested, path.as_deref()) {
        let basename = program
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| requested.to_string());
        let stem = program
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| basename.clone());
        return Ok(ResolvedApp {
            requested: requested.to_string(),
            program,
            args: Vec::new(),
            keys: clean_keys(vec![basename, stem, requested.to_string()]),
            source: "path".to_string(),
            desktop_name: None,
        });
    }

    Err(unresolved(requested, &roots))
}

/// The refusal an unresolvable app produces, with the near misses that help the model
/// correct itself instead of guessing again.
fn unresolved(requested: &str, roots: &[PathBuf]) -> LaunchFailure {
    let wanted = normalize(requested);
    let mut suggestions: Vec<String> = Vec::new();
    for entry in read_desktop_entries(roots) {
        let matches = desktop_strong_keys(&entry).iter().any(|key| {
            let key = normalize(key);
            !key.is_empty() && (key.contains(&wanted) || wanted.contains(&key))
        });
        if matches && !suggestions.contains(&entry.name) {
            suggestions.push(entry.name.clone());
        }
        if suggestions.len() >= 5 {
            break;
        }
    }
    let hint = if suggestions.is_empty() {
        String::new()
    } else {
        format!(" Installed apps with similar names: {}.", suggestions.join(", "))
    };
    LaunchFailure::Refused {
        reason: format!(
            "no application named {requested:?} was found in the XDG desktop entries (searched \
             {}) or on PATH.{hint}",
            roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<String>>()
                .join(", ")
        ),
        alternative: "call list_apps() to see the apps that are already running, or pass an \
                      installed desktop entry name or an executable name or path"
            .to_string(),
    }
}

/// Launch an app, or raise the instance that is already running.
pub fn launch(app: &str) -> Result<LaunchOutcome, LaunchFailure> {
    let resolved = resolve(app)?;

    // The requested name is a matching key in its own right: list_apps() reports an app id
    // that is a WM_CLASS, and a caller that echoes that id back must be deduplicated even
    // when the desktop entry spells the name differently.
    let mut keys = resolved.keys.clone();
    for candidate in [normalize(&resolved.requested), normalize(app)] {
        if candidate.len() >= 2 && !keys.contains(&candidate) {
            keys.push(candidate);
        }
    }

    // Deduplicate before doing anything else. This is the whole point of the module: a
    // second instance of a running app is a worse outcome than a slightly stale window
    // handle, so the existing window wins.
    let existing = window::list_windows().map_err(LaunchFailure::Failed)?;
    if let Some(found) = pick_window(&existing, &keys, &resolved.program) {
        let note = window::activate_window(found.id)
            .map(|note| {
                format!("raised the running instance instead of starting a second one: {note}")
            })
            .unwrap_or_else(|error| {
                format!("the already running instance could not be raised: {error}")
            });
        return Ok(LaunchOutcome {
            launched: false,
            already_running: true,
            window: Some(found),
            note: Some(note),
            resolved,
        });
    }

    spawn_detached(&resolved.program, &resolved.args).map_err(|error| {
        LaunchFailure::Failed(anyhow!(
            "could not start {}: {error}",
            resolved.program.display()
        ))
    })?;

    // The window may take a while to appear; wait for one that matches, but never mistake
    // an unrelated window for it.
    let wait = window_wait();
    let deadline = Instant::now() + wait;
    loop {
        let windows = window::list_windows().map_err(LaunchFailure::Failed)?;
        if let Some(found) = pick_window(&windows, &keys, &resolved.program) {
            let note = window::activate_window(found.id).ok();
            return Ok(LaunchOutcome {
                launched: true,
                already_running: false,
                window: Some(found),
                note,
                resolved,
            });
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(POLL_INTERVAL.min(remaining));
    }

    // Started, but the window is not here yet. Saying so is the honest answer: the app is
    // very likely still coming up, and a fake window would send the model after a handle
    // that does not exist.
    let note = format!(
        "{} was started but no matching window appeared within {} ms; it may still be \
         starting, so call list_windows() again before concluding it failed",
        resolved.program.display(),
        wait.as_millis()
    );
    Ok(LaunchOutcome {
        launched: true,
        already_running: false,
        window: None,
        note: Some(note),
        resolved,
    })
}

/// Pick the window that belongs to the resolved app.
///
/// Matching is exact on the normalized keys, never a substring: a substring match would let
/// an app called "code" claim the windows of "code-insiders", and raising the wrong window
/// is the failure mode deduplication exists to avoid. The process path is the fallback for
/// apps whose WM_CLASS has nothing to do with their name (Electron apps launched through a
/// wrapper are the common case).
pub fn pick_window(windows: &[X11Window], keys: &[String], program: &Path) -> Option<X11Window> {
    let program = canonical(program);
    let mut best: Option<X11Window> = None;
    for candidate in windows {
        if candidate.override_redirect {
            continue;
        }
        if !window_matches(candidate, keys, &program) {
            continue;
        }
        // Prefer a window the operator can actually see, then the focused one, then
        // whatever came first: a hidden or iconic window is a worse answer than a visible
        // one, but it is still a better answer than starting a second instance.
        let better = match &best {
            None => true,
            Some(current) => {
                (current.hidden && !candidate.hidden)
                    || (current.hidden == candidate.hidden && !current.focused && candidate.focused)
            }
        };
        if better {
            best = Some(candidate.clone());
        }
    }
    best
}

fn window_matches(candidate: &X11Window, keys: &[String], program: &Path) -> bool {
    for name in [
        candidate.wm_class.as_deref(),
        candidate.wm_instance.as_deref(),
        Some(candidate.app.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        let name = normalize(name);
        if name.len() >= 2 && keys.iter().any(|key| key == &name) {
            return true;
        }
    }
    if let Some(pid) = candidate.pid {
        if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) {
            if canonical(&exe) == program {
                return true;
            }
        }
    }
    false
}

/// Start a program detached from this process.
///
/// setsid puts the app in its own session so it keeps running after the helper exits and
/// survives a terminal hangup, and the intermediate fork guarantees the helper is not the
/// app's parent: the middle process exits immediately and the app is reparented to init.
/// Without the second fork the helper would have to reap the app, and waiting on a
/// long-lived application would block the request loop.
///
/// The standard streams are /dev/null on purpose. The helper speaks JSONL on stdin and
/// stdout; an app that inherited them would write its own output into the protocol stream
/// and the host would read it as a malformed reply.
fn spawn_detached(program: &Path, args: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt as _;

    let mut command = Command::new(program);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            // Both calls are async-signal-safe, which is the only kind of work allowed
            // between fork and exec in a process that has threads (the helper does).
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            match libc::fork() {
                -1 => Err(std::io::Error::last_os_error()),
                // The grandchild carries on into the exec that Command performs.
                0 => Ok(()),
                // The middle process leaves at once, so the grandchild is orphaned and
                // reparented to init rather than to the helper. _exit skips the parent's
                // atexit handlers, which must not run twice.
                _ => libc::_exit(0),
            }
        });
    }
    let mut middle = command.spawn()?;
    // Reaping the middle process is what keeps it from lingering as a zombie, and it
    // returns immediately because the middle process exits before the app even starts.
    let _ = middle.wait();
    Ok(())
}

/// The process group of a pid, used to prove a launched app is detached.
pub fn process_group(pid: u32) -> Option<i32> {
    let group = unsafe { libc::getpgid(pid as libc::pid_t) };
    (group >= 0).then_some(group)
}

/// The session of a pid, used to prove a launched app is detached.
pub fn session_of(pid: u32) -> Option<i32> {
    let session = unsafe { libc::getsid(pid as libc::pid_t) };
    (session >= 0).then_some(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_codes_are_dropped_and_quotes_are_honoured() {
        let (program, args) = parse_exec("/usr/bin/firefox %U").unwrap();
        assert_eq!(program, "/usr/bin/firefox");
        assert!(args.is_empty(), "a field code is not an argument: {args:?}");

        let (program, args) = parse_exec("\"/opt/My App/bin/run\" --flag \"two words\" %f").unwrap();
        assert_eq!(program, "/opt/My App/bin/run", "quotes group the program");
        assert_eq!(args, vec!["--flag", "two words"], "quotes group an argument");
    }

    #[test]
    fn a_double_percent_is_a_literal_percent_not_a_field_code() {
        let (_, args) = parse_exec("xterm -T 100%%").unwrap();
        assert_eq!(args, vec!["-T", "100%"]);
        // A lone %% is a literal percent argument, not nothing.
        let (program, args) = parse_exec("app %%").unwrap();
        assert_eq!(program, "app");
        assert_eq!(args, vec!["%"]);
    }

    #[test]
    fn every_specified_field_code_is_removed() {
        for code in ['f', 'F', 'u', 'U', 'd', 'D', 'n', 'N', 'i', 'c', 'k', 'v', 'm'] {
            let line = format!("app -x %{code}");
            let (_, args) = parse_exec(&line).unwrap();
            assert_eq!(args, vec!["-x"], "a %{code} code must not reach the app");
        }
    }

    #[test]
    fn an_exec_line_without_a_program_is_an_error_not_a_panic() {
        assert!(parse_exec("%U %f").is_err());
        assert!(parse_exec("   ").is_err());
    }

    #[test]
    fn a_desktop_entry_is_parsed_from_its_entry_group_only() {
        let content = "[Desktop Entry]\n\
                       Type=Application\n\
                       Name=Widget\n\
                       Exec=/usr/bin/widget %u\n\
                       StartupWMClass=Widget\n\
                       Name[de]=Dings\n\
                       \n\
                       [Desktop Action new]\n\
                       Name=Ignored\n\
                       Exec=/usr/bin/other\n";
        let entry = parse_desktop_entry("widget", Path::new("/x/widget.desktop"), content).unwrap();
        assert_eq!(entry.name, "Widget", "the localized name must not win");
        assert_eq!(entry.exec, "/usr/bin/widget %u");
        assert_eq!(entry.startup_wm_class.as_deref(), Some("Widget"));
        assert!(!entry.terminal);
    }

    #[test]
    fn entries_a_desktop_shell_would_hide_are_not_launchable() {
        for content in [
            "Type=Application\nName=Hidden\nExec=/bin/true\nHidden=true\n",
            "Type=Application\nName=Gone\nExec=/bin/true\nNoDisplay=true\n",
            "Type=Link\nName=Link\nExec=/bin/true\n",
            "Type=Application\nName=Missing\nExec=/bin/true\nTryExec=/definitely/not/here\n",
            "Type=Application\nName=NoExec\n",
        ] {
            assert!(
                parse_desktop_entry("x", Path::new("/x/x.desktop"), content).is_none(),
                "must not be launchable: {content}"
            );
        }
    }

    #[test]
    fn the_search_order_puts_the_user_before_the_system() {
        let roots = search_roots_from(Some("/home/me/.local/share"), Some("/usr/share:/opt/share"));
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/home/me/.local/share/applications"),
                PathBuf::from("/usr/share/applications"),
                PathBuf::from("/opt/share/applications"),
            ]
        );
        // The specification's default when XDG_DATA_DIRS is unset.
        let roots = search_roots_from(Some("/d"), None);
        assert_eq!(roots[1], PathBuf::from("/usr/local/share/applications"));
        assert_eq!(roots[2], PathBuf::from("/usr/share/applications"));
        // Duplicates are collapsed so the same entry is never considered twice.
        let roots = search_roots_from(Some("/usr/share"), Some("/usr/share"));
        assert_eq!(roots, vec![PathBuf::from("/usr/share/applications")]);
    }

    #[test]
    fn matching_is_case_insensitive_and_prefers_name_over_exec() {
        let root = std::env::temp_dir().join(format!("cua-launch-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("by-exec.desktop"),
            "[Desktop Entry]\nType=Application\nName=Unrelated\nExec=/usr/bin/exec-only-name\n",
        )
        .unwrap();
        std::fs::write(
            root.join("by-name.desktop"),
            "[Desktop Entry]\nType=Application\nName=Target Name\nExec=/usr/bin/other\n",
        )
        .unwrap();

        let roots = vec![root.clone()];
        // A Name match wins even though the exec entry is read first and sorts earlier.
        let entry = find_desktop_entry_in(&roots, "target name").unwrap();
        assert_eq!(entry.id, "by-name");
        let entry = find_desktop_entry_in(&roots, "TARGET NAME").unwrap();
        assert_eq!(entry.id, "by-name", "matching is case-insensitive");
        // The exec basename is the weaker criterion, and is only reached when no Name,
        // StartupWMClass or file id matched.
        let entry = find_desktop_entry_in(&roots, "exec-only-name").unwrap();
        assert_eq!(entry.id, "by-exec", "an exec basename is the weaker match");
        assert!(find_desktop_entry_in(&roots, "nothing-like-this").is_none());
        // A group header is required: without it there is no [Desktop Entry] group to read,
        // so the file must not be mistaken for a launchable entry.
        std::fs::write(
            root.join("headerless.desktop"),
            "Type=Application\nName=Headerless\nExec=/usr/bin/headerless\n",
        )
        .unwrap();
        assert!(
            find_desktop_entry_in(&roots, "headerless").is_none(),
            "an entry outside [Desktop Entry] must not resolve"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn executable_detection_requires_the_execute_bit() {
        let root = std::env::temp_dir().join(format!("cua-launch-exec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let plain = root.join("not-executable");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        assert!(!is_executable(&plain));
        assert!(!is_executable(&root), "a directory is not a program");
        assert!(!is_executable(&root.join("absent")));
        assert!(is_executable(Path::new("/bin/sh")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_program_is_resolved_through_path_and_by_path() {
        let dir = std::env::temp_dir().join(format!("cua-launch-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("widget");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!("{}:/nonexistent", dir.display());
        assert_eq!(
            program_in_path("widget", Some(&path)).as_deref(),
            Some(program.as_path())
        );
        assert_eq!(
            program_in_path(&program.display().to_string(), None).as_deref(),
            Some(program.as_path())
        );
        assert!(program_in_path("widget", Some("/nonexistent")).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalization_folds_the_separators_a_wm_class_differs_by() {
        assert_eq!(normalize("Clash Verge"), normalize("Clash-verge"));
        assert_eq!(normalize("QQ"), "qq");
        assert_eq!(normalize("org.gnome.Terminal"), "orggnometerminal");
    }

    #[test]
    fn the_window_wait_is_bounded_and_ignores_junk() {
        let previous = std::env::var(TIMEOUT_ENV).ok();

        std::env::set_var(TIMEOUT_ENV, "1500");
        assert_eq!(window_wait(), Duration::from_millis(1500));
        std::env::set_var(TIMEOUT_ENV, "1");
        assert_eq!(window_wait(), Duration::from_millis(MIN_TIMEOUT_MS));
        std::env::set_var(TIMEOUT_ENV, "999999");
        assert_eq!(window_wait(), Duration::from_millis(MAX_TIMEOUT_MS));
        std::env::set_var(TIMEOUT_ENV, "not-a-number");
        assert_eq!(window_wait(), Duration::from_millis(DEFAULT_TIMEOUT_MS));
        std::env::remove_var(TIMEOUT_ENV);
        assert_eq!(window_wait(), Duration::from_millis(DEFAULT_TIMEOUT_MS));

        match previous {
            Some(value) => std::env::set_var(TIMEOUT_ENV, value),
            None => std::env::remove_var(TIMEOUT_ENV),
        }
    }

    #[test]
    fn an_empty_app_name_is_refused_rather_than_resolved_to_something() {
        let error = resolve("   ").unwrap_err();
        match error {
            LaunchFailure::Refused { reason, alternative } => {
                assert!(reason.contains("empty"), "{reason}");
                assert!(!alternative.is_empty());
            }
            LaunchFailure::Failed(error) => panic!("must be a refusal, not an error: {error}"),
        }
    }
}
