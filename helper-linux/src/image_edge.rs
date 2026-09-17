//! The DSH-only maximum-image-edge knob, shared by both screenshot paths.
//!
//! The official helper has no max-edge concept at all: `parity/official-constants.json`
//! records 0 hits for `maxEdge`/`maxImageEdge`/`1280`/`1568` across its 19,738-string
//! table. DSH therefore carries this as an extension knob, and it must stay strictly
//! opt-in:
//!
//! * `maxImageEdge` unset or `0` — no environment variable is exported and nothing here
//!   changes a pixel, so the helper behaves exactly like the official one (decision D-E).
//! * `maxImageEdge` a finite positive value — the plugin exports
//!   `DSH_COMPUTER_USE_MAX_IMAGE_EDGE`, and this module caps the longest edge of the
//!   returned image.
//!
//! The cap only ever shrinks: an image that already fits is returned untouched, because
//! upscaling a screenshot would spend tokens to invent pixels the model cannot trust.
//!
//! It travels through the environment rather than argv because the official helper's
//! hand-written parser aborts on unknown flags — see `Sidecar#spawnNative` in
//! `src/sidecar.js`. Keeping the whole knob in this one module is what lets the X11
//! capture path and the GNOME/portal path agree on the semantics by construction.

/// The environment variable the plugin sets, and the only name this module reads.
pub const MAX_IMAGE_EDGE_ENV: &str = "DSH_COMPUTER_USE_MAX_IMAGE_EDGE";

/// Parse the knob. Returns `None` for absent, empty, non-numeric, non-positive and
/// sub-1 values — every one of those means "no cap", which is the official behaviour.
///
/// The value is read leniently as a float and truncated, so `"900.5"` means a 900-pixel
/// cap instead of being silently discarded: a knob the user set must never be ignored
/// just because the plugin and the helper disagree about numeric types.
pub fn max_image_edge_from_value(raw: Option<&str>) -> Option<u32> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let value = raw.parse::<f64>().ok()?;
    if !value.is_finite() || value < 1.0 {
        return None;
    }
    // `as` saturates, so an absurdly large cap becomes u32::MAX — which
    // `scaled_dimensions` then reports as "already fits" rather than overflowing.
    Some(value.trunc() as u32)
}

/// The configured cap, or `None` when the plugin did not opt in.
pub fn max_image_edge_from_env() -> Option<u32> {
    max_image_edge_from_value(std::env::var(MAX_IMAGE_EDGE_ENV).ok().as_deref())
}

/// The size an image should be returned at so its longest edge is at most `max_edge`.
///
/// `None` means "return it at its natural size": the image already fits, or either
/// dimension is zero (an empty capture is the caller's error to report, not a scaling
/// decision). The aspect ratio is preserved and neither dimension ever grows.
pub fn scaled_dimensions(width: u32, height: u32, max_edge: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || max_edge == 0 {
        return None;
    }
    let longest = width.max(height);
    if longest <= max_edge {
        return None;
    }
    let scale = f64::from(max_edge) / f64::from(longest);
    // Rounding half-up keeps the longest edge at exactly `max_edge` in the common case,
    // and the clamp guards the degenerate ones (a 2x1 image capped at 1 lands on 1x1).
    let target_width = ((f64::from(width) * scale).round() as u32).clamp(1, width);
    let target_height = ((f64::from(height) * scale).round() as u32).clamp(1, height);
    Some((target_width, target_height))
}

/// Serializes the tests that set `MAX_IMAGE_EDGE_ENV`.
///
/// The variable is process-global and cargo runs a binary's tests on threads, so every
/// test module that touches it must take *this one* lock — a per-module lock would leave
/// the two modules racing each other.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `f` with the knob set to `value`, restoring the previous value afterwards.
#[cfg(test)]
pub(crate) fn with_env<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = std::env::var(MAX_IMAGE_EDGE_ENV).ok();
    match value {
        Some(value) => std::env::set_var(MAX_IMAGE_EDGE_ENV, value),
        None => std::env::remove_var(MAX_IMAGE_EDGE_ENV),
    }
    let result = f();
    match previous {
        Some(value) => std::env::set_var(MAX_IMAGE_EDGE_ENV, value),
        None => std::env::remove_var(MAX_IMAGE_EDGE_ENV),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_positive_value_is_the_only_thing_that_opts_in() {
        assert_eq!(max_image_edge_from_value(Some("1280")), Some(1280));
        assert_eq!(max_image_edge_from_value(Some("  1568 ")), Some(1568));
        assert_eq!(max_image_edge_from_value(Some("1")), Some(1));

        // Every one of these is "no cap", i.e. the official behaviour.
        assert_eq!(max_image_edge_from_value(None), None);
        assert_eq!(max_image_edge_from_value(Some("")), None);
        assert_eq!(max_image_edge_from_value(Some("   ")), None);
        assert_eq!(max_image_edge_from_value(Some("0")), None);
        assert_eq!(max_image_edge_from_value(Some("-5")), None);
        assert_eq!(max_image_edge_from_value(Some("nonsense")), None);
        assert_eq!(max_image_edge_from_value(Some("inf")), None);
        assert_eq!(max_image_edge_from_value(Some("NaN")), None);
        // A fractional cap is truncated rather than ignored, and anything under 1 is no cap.
        assert_eq!(max_image_edge_from_value(Some("900.5")), Some(900));
        assert_eq!(max_image_edge_from_value(Some("0.5")), None);
        assert_eq!(
            max_image_edge_from_value(Some("99999999999999")),
            Some(u32::MAX)
        );
    }

    #[test]
    fn an_image_that_already_fits_is_never_touched() {
        // The whole point of "只缩不放": 1920x1200 on a 1920 default must come back as
        // 1920x1200, which is exactly the case the real-hardware report complained about.
        assert_eq!(scaled_dimensions(1920, 1200, 1920), None);
        assert_eq!(scaled_dimensions(1920, 1200, 2000), None);
        assert_eq!(scaled_dimensions(300, 200, 300), None);
        assert_eq!(scaled_dimensions(1, 1, 8), None);
    }

    #[test]
    fn the_longest_edge_lands_on_the_cap_and_the_ratio_is_kept() {
        assert_eq!(scaled_dimensions(3840, 2160, 1920), Some((1920, 1080)));
        assert_eq!(scaled_dimensions(1920, 1200, 960), Some((960, 600)));
        assert_eq!(scaled_dimensions(300, 200, 100), Some((100, 67)));
        // The cap applies to whichever edge is longer, not just the width.
        assert_eq!(scaled_dimensions(200, 400, 100), Some((50, 100)));
    }

    #[test]
    fn the_result_never_exceeds_the_cap_and_never_grows() {
        for width in [1u32, 2, 3, 7, 99, 100, 101, 640, 1920, 4000] {
            for height in [1u32, 2, 3, 7, 99, 100, 101, 640, 1200, 4000] {
                for max_edge in [1u32, 2, 5, 100, 1280] {
                    let Some((out_width, out_height)) = scaled_dimensions(width, height, max_edge)
                    else {
                        // "No scaling" is only allowed when the image already fits.
                        assert!(width.max(height) <= max_edge, "{width}x{height} capped at {max_edge}");
                        continue;
                    };
                    assert!(out_width <= width && out_height <= height, "never grows: {width}x{height}");
                    assert!(out_width >= 1 && out_height >= 1, "never collapses to zero");
                    assert!(
                        out_width.max(out_height) <= max_edge,
                        "{out_width}x{out_height} must fit within {max_edge}",
                    );
                }
            }
        }
    }

    #[test]
    fn a_degenerate_image_is_not_a_scaling_decision() {
        assert_eq!(scaled_dimensions(0, 100, 50), None);
        assert_eq!(scaled_dimensions(100, 0, 50), None);
        assert_eq!(scaled_dimensions(100, 100, 0), None);
    }
}
