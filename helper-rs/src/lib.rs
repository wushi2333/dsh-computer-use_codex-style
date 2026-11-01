//! Optional library surface. The stdio helper binary is `src/main.rs`.

pub mod app_catalog;
pub mod assist;
pub mod audio;
pub mod capture;
pub mod desktop;
pub mod dpi;
pub mod enum_windows;
pub mod images;
pub mod ime;
pub mod input;
pub mod interrupt;
pub mod notify;
pub mod overlay;
pub mod pipe;
pub mod policy;
pub mod prompt;
pub mod protocol;
pub mod state;
pub mod tools;
pub mod uia;

pub use capture::{capture_hwnd, CaptureFrame};
