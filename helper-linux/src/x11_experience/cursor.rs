//! The synthesized pointer: its geometry, its press animation, and where it goes.
//!
//! Windows draws this with Direct2D in `helper-rs/src/overlay/mod.rs`; here it is a
//! filled polygon the X server renders. Keeping the geometry as plain numbers makes
//! the whole thing testable without a display -- the X11 layer only turns vertices
//! into `fill_polygon` calls.

use std::time::{Duration, Instant};

/// The pressed state is a 550 ms scale transition whose midpoint is 275 ms in
/// (official `schedule cursor press release` / `release cursor pressed state`).
pub const PRESS_MS: u64 = 550;
pub const PRESS_MID_MS: u64 = 275;
/// How small the sprite shrinks at the bottom of the press (VIS-04).
pub const PRESS_SCALE: f32 = 0.82;
/// Glow/shadow geometry, in device pixels at scale 1.0.
pub const DEFAULT_SIZE: i32 = 28;
pub const SHADOW_OFFSET: i32 = 2;

/// A cursor sprite: the outline the operator sees, plus the drop shadow behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct Sprite {
    pub width: i32,
    pub height: i32,
    /// Hot spot in sprite-local pixels: this is the point that tracks the pointer.
    pub hot: (i32, i32),
    /// Filled shape, clockwise, in sprite-local pixels.
    pub outline: Vec<(i16, i16)>,
    /// Shadow shape, already offset by `SHADOW_OFFSET`.
    pub shadow: Vec<(i16, i16)>,
    /// Press animation scale in [PRESS_SCALE, 1.0].
    pub scale: f32,
}

/// The classic arrow. Vertices are fractions of the sprite box, so the same shape
/// works at every scale.
fn arrow_vertices(size: f32) -> Vec<(i16, i16)> {
    let p = |x: f32, y: f32| ((x * size).round() as i16, (y * size).round() as i16);
    vec![
        p(0.08, 0.04),   // tip (hot spot)
        p(0.08, 0.96),   // tail bottom
        p(0.32, 0.72),   // notch left
        p(0.50, 1.00),   // flare
        p(0.64, 0.92),
        p(0.46, 0.66),   // notch right
        p(0.76, 0.62),
    ]
}

/// The press animation: down over the first half, back to 1.0 by `PRESS_MS`.
///
/// Pure and total: any elapsed value produces a scale in `[PRESS_SCALE, 1.0]`, so a
/// torn frame cannot draw a cursor of zero size.
pub fn press_scale(elapsed: Duration) -> f32 {
    let ms = elapsed.as_millis() as u64;
    if ms >= PRESS_MS {
        return 1.0;
    }
    let (from, to, span, offset) = if ms < PRESS_MID_MS {
        (1.0f32, PRESS_SCALE, PRESS_MID_MS as f32, 0.0f32)
    } else {
        (PRESS_SCALE, 1.0f32, (PRESS_MS - PRESS_MID_MS) as f32, PRESS_MID_MS as f32)
    };
    let progress = ((ms as f32) - offset) / span;
    // Smoothstep, matching the eased official Scale transition rather than a
    // linear ramp, so the release does not look like a snap.
    let eased = progress * progress * (3.0 - 2.0 * progress);
    (from + (to - from) * eased).clamp(PRESS_SCALE, 1.0)
}

/// Build the sprite for a given animation state.
pub fn sprite(pressed_at: Option<Instant>, now: Instant) -> Sprite {
    let scale = match pressed_at {
        Some(at) => press_scale(now.saturating_duration_since(at)),
        None => 1.0,
    };
    let size = (DEFAULT_SIZE as f32 * scale).max(8.0);
    let outline = arrow_vertices(size);
    let shadow = outline
        .iter()
        .map(|(x, y)| (x.saturating_add(SHADOW_OFFSET as i16), y.saturating_add(SHADOW_OFFSET as i16)))
        .collect();
    let width = (size.round() as i32 + SHADOW_OFFSET).max(1);
    let height = (size.round() as i32 + SHADOW_OFFSET).max(1);
    let hot = ((0.08 * size).round() as i32, (0.04 * size).round() as i32);
    Sprite { width, height, hot, outline, shadow, scale }
}

/// Where the sprite's top-left goes so that its hot spot sits on the pointer while
/// staying fully on screen.
pub fn window_origin(pointer: (i32, i32), sprite: &Sprite, screen: (i32, i32)) -> (i32, i32) {
    let x = pointer.0 - sprite.hot.0;
    let y = pointer.1 - sprite.hot.1;
    let max_x = (screen.0 - sprite.width).max(0);
    let max_y = (screen.1 - sprite.height).max(0);
    (x.clamp(0, max_x), y.clamp(0, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn an_unpressed_sprite_is_full_size() {
        let s = sprite(None, at(0));
        assert_eq!(s.scale, 1.0);
        assert_eq!(s.outline.len(), 7);
        assert_eq!(s.shadow.len(), s.outline.len());
    }

    #[test]
    fn the_press_bottoms_out_at_the_midpoint_and_recovers() {
        assert!((press_scale(Duration::from_millis(0)) - 1.0).abs() < 1e-6);
        assert!((press_scale(Duration::from_millis(PRESS_MID_MS)) - PRESS_SCALE).abs() < 1e-6);
        assert!((press_scale(Duration::from_millis(PRESS_MS)) - 1.0).abs() < 1e-6);
        assert!((press_scale(Duration::from_millis(10_000)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_press_animation_is_monotonic_on_each_half() {
        let mut previous = 1.0f32;
        for ms in 0..PRESS_MID_MS {
            let s = press_scale(Duration::from_millis(ms));
            assert!(s <= previous + 1e-6, "press went back up at {ms}ms: {s} > {previous}");
            previous = s;
        }
        let mut previous = PRESS_SCALE;
        for ms in PRESS_MID_MS..=PRESS_MS {
            let s = press_scale(Duration::from_millis(ms));
            assert!(s >= previous - 1e-6, "release went back down at {ms}ms: {s} < {previous}");
            previous = s;
        }
    }

    #[test]
    fn the_scale_stays_inside_its_bounds_for_every_elapsed_value() {
        for ms in 0..1200 {
            let s = press_scale(Duration::from_millis(ms));
            assert!((PRESS_SCALE..=1.0).contains(&s), "{ms}ms produced {s}");
        }
    }

    #[test]
    fn a_pressed_sprite_is_smaller_and_its_vertices_track_the_scale() {
        let full = sprite(None, at(0));
        let pressed = sprite(Some(at(0)), at(PRESS_MID_MS));
        assert!(pressed.scale < full.scale);
        assert!(pressed.outline[1].1 < full.outline[1].1, "the tail must move in with the sprite");
    }

    #[test]
    fn the_shadow_trails_the_outline_by_the_shadow_offset() {
        let s = sprite(None, at(0));
        for ((ox, oy), (sx, sy)) in s.outline.iter().zip(s.shadow.iter()) {
            assert_eq!(*sx, ox + SHADOW_OFFSET as i16);
            assert_eq!(*sy, oy + SHADOW_OFFSET as i16);
        }
    }

    #[test]
    fn the_hot_spot_sits_on_the_arrow_tip() {
        let s = sprite(None, at(0));
        assert_eq!(s.outline[0], (s.hot.0 as i16, s.hot.1 as i16));
    }

    #[test]
    fn the_sprite_is_placed_under_the_pointer_and_kept_on_screen() {
        let s = sprite(None, at(0));
        let (x, y) = window_origin((500, 400), &s, (1280, 800));
        assert_eq!(x + s.hot.0, 500);
        assert_eq!(y + s.hot.1, 400);

        // A pointer in the bottom-right corner must not push the sprite off screen.
        let (x, y) = window_origin((1279, 799), &s, (1280, 800));
        assert!(x + s.width <= 1280 && y + s.height <= 800, "sprite escaped the screen at {x},{y}");

        // Nor may a pointer at the origin produce a negative position.
        let (x, y) = window_origin((0, 0), &s, (1280, 800));
        assert!(x >= 0 && y >= 0);
    }

    #[test]
    fn a_screen_smaller_than_the_sprite_still_yields_a_usable_origin() {
        let s = sprite(None, at(0));
        let (x, y) = window_origin((40, 40), &s, (10, 10));
        assert_eq!((x, y), (0, 0));
    }
}
