#[cfg(target_os = "linux")]
use mimalloc::MiMalloc;

#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// The binary is a thin front end over the library: both the JSONL helper and the
// original MCP/CLI entry points come from the same crate, so there is exactly one
// definition of every backend rather than a second module tree compiled into the bin.
use dsh_computer_use::{
    abs_pointer, atspi_tree, diagnostics, gnome_extension, helper, screenshot, server, windows,
};

use anyhow::{Context, Result};

#[derive(Debug, PartialEq, Eq)]
pub struct CliArgs {
    pub parent_pid: Option<u32>,
    pub subcommand: Option<String>,
    pub rest: Vec<String>,
}

/// Parse CLI arguments, separating optional `--parent-pid <pid>` / `--parent-pid=<pid>`
/// from the subcommand and its arguments.
pub fn parse_cli_args<I, T>(args: I) -> CliArgs
where
    I: IntoIterator<Item = T>,
    T: Into<String>,
{
    let raw: Vec<String> = args.into_iter().map(Into::into).collect();
    let mut parent_pid = None;
    let mut non_parent_args = Vec::new();
    let mut i = 1; // skip binary name argv[0]
    while i < raw.len() {
        if raw[i] == "--parent-pid" {
            if i + 1 < raw.len() {
                if let Ok(pid) = raw[i + 1].parse::<u32>() {
                    parent_pid = Some(pid);
                }
                i += 2;
                continue;
            }
        } else if let Some(val) = raw[i].strip_prefix("--parent-pid=") {
            if let Ok(pid) = val.parse::<u32>() {
                parent_pid = Some(pid);
            }
            i += 1;
            continue;
        }
        non_parent_args.push(raw[i].clone());
        i += 1;
    }

    let subcommand = non_parent_args.first().cloned();
    let rest = if !non_parent_args.is_empty() {
        non_parent_args[1..].to_vec()
    } else {
        Vec::new()
    };

    CliArgs {
        parent_pid,
        subcommand,
        rest,
    }
}

/// Monitor parent PID by polling; if the parent process exits, terminate self.
/// Semantics align with computer_use/rpc.py _watch_parent.
fn watch_parent(pid: u32) {
    if pid == 0 {
        return;
    }
    std::thread::Builder::new()
        .name("cu-parent-watcher".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                // libc::kill with signal 0 checks for process existence without sending a signal
                if unsafe { libc::kill(pid as i32, 0) } != 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::ESRCH) {
                        std::process::exit(0);
                    }
                }
            }
        })
        .ok();
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    diagnostics::hydrate_session_bus_env();

    let cli = parse_cli_args(std::env::args());

    if let Some(pid) = cli.parent_pid {
        watch_parent(pid);
    }

    match cli.subcommand.as_deref() {
        // The plugin's stdio JSONL helper protocol. This is the default entry point
        // because the plugin's JS sidecar spawns the binary with no subcommand.
        Some("helper") | None => {
            let result = helper::serve().await;
            helper::flush_stdout();
            result
        }
        Some("mcp") => server::serve_mcp().await,
        Some("doctor") => {
            let report = diagnostics::doctor_report();
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .context("failed to serialize doctor report")?
            );
            Ok(())
        }
        Some("setup") => {
            let report = diagnostics::setup_accessibility_report();
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .context("failed to serialize setup report")?
            );
            Ok(())
        }
        Some("apps") => {
            let apps = atspi_tree::list_accessible_apps(50).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&apps)
                    .context("failed to serialize accessible apps")?
            );
            Ok(())
        }
        Some("state") => {
            let app_name_or_bundle_identifier = cli.rest.get(0).cloned();
            let (max_nodes, max_depth) = atspi_tree::snapshot_limits(None, None);
            let nodes = atspi_tree::snapshot_tree(
                app_name_or_bundle_identifier.as_deref(),
                None,
                max_nodes,
                max_depth,
            )
            .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&nodes)
                    .context("failed to serialize accessibility tree")?
            );
            Ok(())
        }
        // Hidden dev command: empirically test the absolute pointer.
        // `abs-test X Y` moves to logical (X,Y) and left-clicks, sizing the
        // device to the live screenshot dimensions.
        Some("abs-test") => {
            let x: i32 = cli
                .rest
                .get(0)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let y: i32 = cli
                .rest
                .get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let cap = screenshot::capture_screenshot_raw().await?;
            eprintln!("desktop logical size: {}x{}", cap.width, cap.height);
            let mut p = abs_pointer::AbsPointer::create(cap.width as i32, cap.height as i32)?;
            p.click(x, y, abs_pointer::PointerButton::Left, 1)?;
            println!(
                "{}",
                serde_json::json!({"ok": true, "x": x, "y": y, "w": cap.width, "h": cap.height})
            );
            Ok(())
        }
        Some("screenshot") => {
            let capture = screenshot::capture_screenshot().await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "mime_type": capture.mime_type,
                    "source": capture.source,
                    "width": capture.width,
                    "height": capture.height,
                    "coordinate_width": capture.coordinate_width,
                    "coordinate_height": capture.coordinate_height,
                    "scale": capture.scale,
                    "resized": capture.resized,
                    "bytes": capture.bytes,
                    "original_bytes": capture.original_bytes,
                    "max_bytes": capture.max_bytes,
                    "format": capture.format,
                    "quality": capture.quality,
                    "data_url_length": capture.data_url.len()
                }))
                .context("failed to serialize screenshot report")?
            );
            Ok(())
        }
        Some("windows") => {
            let report = match windows::list_windows().await {
                Ok(windows) => {
                    let backend = windows
                        .first()
                        .map(|window| window.backend.as_str())
                        .unwrap_or(windows::GNOME_SHELL_INTROSPECT_BACKEND);
                    serde_json::json!({
                        "backend": backend,
                        "windows": windows,
                        "error": null,
                        "permissions_hint": null,
                    })
                }
                Err(error) => {
                    let error = format!("{error:#}");
                    serde_json::json!({
                        "backend": "unavailable",
                        "windows": [],
                        "error": error,
                        "permissions_hint": windows::window_permission_hint(&error),
                    })
                }
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Some("setup-window-targeting") => {
            let report = gnome_extension::setup_window_targeting_report().await;
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .context("failed to serialize window targeting setup report")?
            );
            Ok(())
        }
        Some("--help") | Some("-h") => {
            print_help();
            Ok(())
        }
        Some(command) => {
            anyhow::bail!(
                "unknown command '{command}'. Expected one of: helper (default), mcp, doctor, setup, apps, state, screenshot, windows, setup-window-targeting"
            );
        }
    }
}

fn print_help() {
    println!(
        "dsh-computer-use (Linux)\n\nThe plugin spawns this binary with no arguments to speak the stdio JSONL helper protocol;\n  dsh-computer-use helper  is the explicit form of the same thing.\n\nUsage:\n  dsh-computer-use [helper]\n  dsh-computer-use mcp\n  dsh-computer-use doctor\n  dsh-computer-use setup\n  dsh-computer-use setup-window-targeting\n  dsh-computer-use apps\n  dsh-computer-use state [APP_NAME]\n  dsh-computer-use screenshot\n  dsh-computer-use windows"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cli_args_empty() {
        let parsed = parse_cli_args(vec!["dsh-computer-use"]);
        assert_eq!(
            parsed,
            CliArgs {
                parent_pid: None,
                subcommand: None,
                rest: vec![],
            }
        );
    }

    #[test]
    fn test_parse_cli_args_parent_pid_space() {
        let parsed = parse_cli_args(vec!["dsh-computer-use", "--parent-pid", "12345"]);
        assert_eq!(
            parsed,
            CliArgs {
                parent_pid: Some(12345),
                subcommand: None,
                rest: vec![],
            }
        );
    }

    #[test]
    fn test_parse_cli_args_parent_pid_equals() {
        let parsed = parse_cli_args(vec!["dsh-computer-use", "--parent-pid=67890"]);
        assert_eq!(
            parsed,
            CliArgs {
                parent_pid: Some(67890),
                subcommand: None,
                rest: vec![],
            }
        );
    }

    #[test]
    fn test_parse_cli_args_parent_pid_with_subcommand() {
        let parsed = parse_cli_args(vec![
            "dsh-computer-use",
            "--parent-pid",
            "100",
            "state",
            "org.gnome.TextEditor",
        ]);
        assert_eq!(
            parsed,
            CliArgs {
                parent_pid: Some(100),
                subcommand: Some("state".to_string()),
                rest: vec!["org.gnome.TextEditor".to_string()],
            }
        );
    }

    #[test]
    fn test_parse_cli_args_subcommand_before_parent_pid() {
        let parsed = parse_cli_args(vec![
            "dsh-computer-use",
            "helper",
            "--parent-pid",
            "200",
        ]);
        assert_eq!(
            parsed,
            CliArgs {
                parent_pid: Some(200),
                subcommand: Some("helper".to_string()),
                rest: vec![],
            }
        );
    }

    #[test]
    fn test_parse_cli_args_abs_test() {
        let parsed = parse_cli_args(vec![
            "dsh-computer-use",
            "abs-test",
            "150",
            "300",
            "--parent-pid=50",
        ]);
        assert_eq!(
            parsed,
            CliArgs {
                parent_pid: Some(50),
                subcommand: Some("abs-test".to_string()),
                rest: vec!["150".to_string(), "300".to_string()],
            }
        );
    }
}
