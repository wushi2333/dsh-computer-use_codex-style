pub const DEFAULT_GNOME_EXTENSION_UUID: &str = "codex-window-control@openai.com";
pub const DEFAULT_DBUS_SERVICE: &str = "com.openai.Codex.WindowControl";
pub const DEFAULT_DBUS_OBJECT_PATH: &str = "/com/openai/Codex/WindowControl";

pub const GNOME_EXTENSION_UUID: &str = match option_env!("CUL_GNOME_EXTENSION_UUID") {
    Some(value) => value,
    None => DEFAULT_GNOME_EXTENSION_UUID,
};

pub const DBUS_SERVICE: &str = match option_env!("CUL_DBUS_SERVICE") {
    Some(value) => value,
    None => DEFAULT_DBUS_SERVICE,
};

pub const DBUS_OBJECT_PATH: &str = match option_env!("CUL_DBUS_OBJECT_PATH") {
    Some(value) => value,
    None => DEFAULT_DBUS_OBJECT_PATH,
};
