pub mod abs_pointer;
pub mod atspi_tree;
mod command_runner;
pub mod cosmic_helper;
pub mod diagnostics;
pub mod gnome_extension;
pub mod helper;
pub mod identity;
pub mod image_edge;
pub mod protocol;
pub mod remote_desktop;
pub mod screenshot;
pub mod server;
pub mod terminal;
pub mod windowing;
pub mod x11;
pub mod x11_experience;
pub mod windows;
pub(crate) mod ydotool;

// The helper layer drives the crate's own rmcp ToolRouter, so the MCP types are part
// of this crate's own surface. Re-exported rather than re-declared so there is exactly
// one rmcp in the build.
pub use rmcp;
