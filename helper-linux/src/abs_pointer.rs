//! Direct uinput **absolute** pointer.
//!
//! ydotool's virtual device is relative-only (`EV=7`: SYN|KEY|REL), so its
//! `--absolute` is faked as "pin-to-corner + relative move", which the
//! compositor then distorts with pointer acceleration and fractional display
//! scaling — clicks land in the wrong place on multi-monitor / HiDPI setups.
//!
//! Here we create our own uinput device that exposes a true `ABS_X`/`ABS_Y`
//! axis whose range equals the **logical desktop size** (the same coordinate
//! space the portal screenshot reports). The compositor maps an absolute
//! device's axis range across the whole logical layout, so `ABS(x, y)` lands at
//! screenshot pixel `(x, y)` regardless of scaling — and with no approval
//! dialog (we already hold `/dev/uinput` access).

use std::thread::sleep;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use evdev::{
    uinput::VirtualDevice, AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode,
    PropType, UinputAbsSetup,
};

pub struct AbsPointer {
    device: VirtualDevice,
    width: i32,
    height: i32,
}

impl AbsPointer {
    /// Create the absolute pointer sized to the logical desktop `width`×`height`
    /// (the portal screenshot dimensions). Blocks ~`settle` ms so libinput picks
    /// the device up before the first event.
    pub fn create(width: i32, height: i32) -> Result<Self> {
        let width = width.max(1);
        let height = height.max(1);
        // value, min, max, fuzz, flat, resolution. resolution=1 unit/px.
        let abs_x =
            UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, AbsInfo::new(0, 0, width, 0, 0, 1));
        let abs_y =
            UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, AbsInfo::new(0, 0, height, 0, 0, 1));
        let keys =
            AttributeSet::from_iter([KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE]);
        // INPUT_PROP_DIRECT marks the device as a direct (absolute) pointer so
        // libinput maps its axes to screen coordinates rather than treating it
        // as a relative touchpad.
        let props = AttributeSet::from_iter([PropType::DIRECT]);

        let device = VirtualDevice::builder()
            .context("uinput builder (is /dev/uinput writable?)")?
            .name("codex-computer-use-linux absolute pointer")
            .with_properties(&props)?
            .with_absolute_axis(&abs_x)?
            .with_absolute_axis(&abs_y)?
            .with_keys(&keys)?
            .build()
            .context("failed to create uinput absolute pointer device")?;

        // Give udev/libinput time to enumerate the new device.
        sleep(Duration::from_millis(500));

        Ok(Self {
            device,
            width,
            height,
        })
    }

    /// Move the pointer to absolute logical coordinates `(x, y)`.
    pub fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        let x = x.clamp(0, self.width);
        let y = y.clamp(0, self.height);
        self.device
            .emit(&[
                InputEvent::new_now(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x),
                InputEvent::new_now(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y),
            ])
            .context("failed to emit absolute motion")?;
        Ok(())
    }

    /// Move to `(x, y)` then press+release `button` `count` times.
    pub fn click(&mut self, x: i32, y: i32, button: PointerButton, count: u32) -> Result<()> {
        self.move_to(x, y)?;
        sleep(Duration::from_millis(30));
        let code = button.key_code();
        for _ in 0..count.max(1) {
            if let Err(error) = self
                .device
                .emit(&[InputEvent::new_now(EventType::KEY.0, code, 1)])
            {
                return Err(self.release_after_error(code, error.into()));
            }
            sleep(Duration::from_millis(30));
            if let Err(error) = self.release_button(code) {
                return Err(self.release_after_error(code, error));
            }
            sleep(Duration::from_millis(40));
        }
        Ok(())
    }

    /// Press at `(start)`, move to `(end)`, release — a drag with `button`.
    pub fn drag(
        &mut self,
        start: (i32, i32),
        end: (i32, i32),
        button: PointerButton,
    ) -> Result<()> {
        let code = button.key_code();
        self.move_to(start.0, start.1)?;
        sleep(Duration::from_millis(30));
        if let Err(error) = self
            .device
            .emit(&[InputEvent::new_now(EventType::KEY.0, code, 1)])
        {
            return Err(self.release_after_error(code, error.into()));
        }
        sleep(Duration::from_millis(40));
        if let Err(error) = self.move_to(end.0, end.1) {
            return Err(self.release_after_error(code, error));
        }
        sleep(Duration::from_millis(40));
        if let Err(error) = self.release_button(code) {
            return Err(self.release_after_error(code, error));
        }
        Ok(())
    }

    fn release_button(&mut self, code: u16) -> Result<()> {
        self.device
            .emit(&[InputEvent::new_now(EventType::KEY.0, code, 0)])
            .context("failed to emit absolute pointer button release")
    }

    fn release_after_error(&mut self, code: u16, error: anyhow::Error) -> anyhow::Error {
        match self.release_button(code) {
            Ok(()) => anyhow!("{error:#}; sent a best-effort button release"),
            Err(release_error) => {
                anyhow!("{error:#}; best-effort button release also failed: {release_error:#}")
            }
        }
    }
}

/// Pointer buttons we can synthesize.
#[derive(Clone, Copy, Debug)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

impl PointerButton {
    pub fn from_name(name: Option<&str>) -> Self {
        match name.unwrap_or("left").to_ascii_lowercase().as_str() {
            "right" => Self::Right,
            "middle" => Self::Middle,
            _ => Self::Left,
        }
    }

    fn key_code(self) -> u16 {
        match self {
            Self::Left => KeyCode::BTN_LEFT.0,
            Self::Right => KeyCode::BTN_RIGHT.0,
            Self::Middle => KeyCode::BTN_MIDDLE.0,
        }
    }
}
