//! Where a new window appears: the pure map from a `Placement` to the
//! platform options the opener hands gpui (`display_id`, `window_bounds`).
//! The opener gathers the facts (the spawner's frame and display, the
//! displays, the primary) and this module decides; nothing here touches a
//! window or the app.

use geode_shell::shell::Placement;
use gpui::{Bounds, DisplayId, Pixels, Point, WindowBounds, point, px};

/// How far a window opened beside another is offset from it, in x and
/// in y, and how far from a display's corner a window relocated to it
/// sits. Logical pixels.
pub const CASCADE: Pixels = px(28.);

/// The platform options for a window at `placement`.
///
/// - `Beside`: the spawner's display, and its frame shifted by [`CASCADE`]
///   in x and y, then clamped so the whole window stays inside that
///   display: an overflow shifts it back by the overflow, and a window
///   larger than the display sits at the display's origin. With no
///   spawner (its window is gone) or a spawner whose display is unknown,
///   nothing or the shifted frame alone is asked for.
/// - `Restored` with a display that is still attached: that display and
///   the recorded frame, `Maximized` when it was. A display no longer
///   attached: the primary instead, with the recorded size at the
///   primary's origin plus [`CASCADE`], so a window recorded on a
///   monitor since unplugged comes back on screen rather than off it.
///   No recorded display: the frame alone, for the platform to place.
/// - `Default`: nothing; the platform places the window.
///
/// Displays are `(id, bounds)` in the global logical coordinate space, so
/// a monitor to the left of the primary has a negative origin and the
/// clamp arithmetic needs no special case for it.
pub fn options_for(
    placement: &Placement,
    spawner: Option<(Bounds<Pixels>, Option<u64>)>,
    displays: &[(u64, Bounds<Pixels>)],
    primary: Option<u64>,
) -> (Option<DisplayId>, Option<WindowBounds>) {
    match placement {
        Placement::Default => (None, None),
        Placement::Beside { .. } => {
            let Some((frame, display)) = spawner else {
                return (None, None);
            };
            let cascaded = Bounds::new(frame.origin + cascade(), frame.size);
            match display.and_then(|d| bounds_of(displays, d).map(|b| (d, b))) {
                Some((d, screen)) => (
                    Some(DisplayId::new(d)),
                    Some(WindowBounds::Windowed(clamp_into(cascaded, screen))),
                ),
                None => (None, Some(WindowBounds::Windowed(cascaded))),
            }
        }
        Placement::Restored {
            display,
            bounds,
            maximized,
        } => {
            let Some(d) = *display else {
                return (None, bounds.map(|b| framed(b, *maximized)));
            };
            if displays.iter().any(|(id, _)| *id == d) {
                return (
                    Some(DisplayId::new(d)),
                    bounds.map(|b| framed(b, *maximized)),
                );
            }
            let screen = primary.and_then(|p| bounds_of(displays, p));
            let relocated = bounds.map(|b| match screen {
                Some(screen) => Bounds::new(screen.origin + cascade(), b.size),
                None => b,
            });
            (
                primary.map(DisplayId::new),
                relocated.map(|b| framed(b, *maximized)),
            )
        }
    }
}

fn cascade() -> Point<Pixels> {
    point(CASCADE, CASCADE)
}

fn bounds_of(displays: &[(u64, Bounds<Pixels>)], id: u64) -> Option<Bounds<Pixels>> {
    displays.iter().find(|(d, _)| *d == id).map(|(_, b)| *b)
}

/// The recorded frame as the window state it was saved in.
fn framed(bounds: Bounds<Pixels>, maximized: bool) -> WindowBounds {
    if maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

/// `window` moved the least distance that keeps it inside `screen`: back
/// by the overflow on each axis, and to the screen's origin when it is
/// larger than the screen on that axis.
fn clamp_into(window: Bounds<Pixels>, screen: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(
            clamp_axis(
                window.origin.x,
                window.size.width,
                screen.origin.x,
                screen.size.width,
            ),
            clamp_axis(
                window.origin.y,
                window.size.height,
                screen.origin.y,
                screen.size.height,
            ),
        ),
        window.size,
    )
}

fn clamp_axis(
    origin: Pixels,
    length: Pixels,
    screen_origin: Pixels,
    screen_length: Pixels,
) -> Pixels {
    let overflow = (origin + length) - (screen_origin + screen_length);
    let shifted = if overflow > px(0.) {
        origin - overflow
    } else {
        origin
    };
    if shifted < screen_origin {
        screen_origin
    } else {
        shifted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::tiling::WindowKey;
    use gpui::size;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    /// The primary at the origin and a second monitor to its left, so a
    /// display with a negative origin is in play.
    fn displays() -> Vec<(u64, Bounds<Pixels>)> {
        vec![
            (1, rect(0., 0., 1920., 1080.)),
            (2, rect(-1600., 0., 1600., 900.)),
        ]
    }

    const BESIDE: Placement = Placement::Beside {
        from: WindowKey(1),
        spawner: None,
    };

    #[test]
    fn beside_cascades_and_clamps_into_the_display() {
        // Room to cascade: the frame moves by CASCADE in x and y.
        assert_eq!(
            options_for(
                &BESIDE,
                Some((rect(100., 100., 800., 600.), Some(1))),
                &displays(),
                Some(1)
            ),
            (
                Some(DisplayId::new(1)),
                Some(WindowBounds::Windowed(rect(128., 128., 800., 600.)))
            )
        );
        // Near the far corner: shifted back by the overflow on each axis
        // (1128 + 800 - 1920 = 8; 528 + 600 - 1080 = 48).
        assert_eq!(
            options_for(
                &BESIDE,
                Some((rect(1100., 500., 800., 600.), Some(1))),
                &displays(),
                Some(1)
            ),
            (
                Some(DisplayId::new(1)),
                Some(WindowBounds::Windowed(rect(1120., 480., 800., 600.)))
            )
        );
        // On the monitor to the left: the clamp works in its negative
        // coordinates (-172 + 800 - 0 = 628 over; 428 + 600 - 900 = 128).
        assert_eq!(
            options_for(
                &BESIDE,
                Some((rect(-200., 400., 800., 600.), Some(2))),
                &displays(),
                Some(1)
            ),
            (
                Some(DisplayId::new(2)),
                Some(WindowBounds::Windowed(rect(-800., 300., 800., 600.)))
            )
        );
        // Larger than the display: the origin is the display's.
        assert_eq!(
            options_for(
                &BESIDE,
                Some((rect(0., 0., 2000., 1200.), Some(1))),
                &displays(),
                Some(1)
            ),
            (
                Some(DisplayId::new(1)),
                Some(WindowBounds::Windowed(rect(0., 0., 2000., 1200.)))
            )
        );
    }

    #[test]
    fn beside_with_no_spawner_asks_for_nothing() {
        assert_eq!(
            options_for(&BESIDE, None, &displays(), Some(1)),
            (None, None)
        );
    }

    #[test]
    fn a_restored_window_on_a_missing_display_opens_on_the_primary() {
        let placement = Placement::Restored {
            display: Some(7),
            bounds: Some(rect(-1500., 100., 800., 600.)),
            maximized: false,
        };
        assert_eq!(
            options_for(&placement, None, &displays(), Some(1)),
            (
                Some(DisplayId::new(1)),
                Some(WindowBounds::Windowed(rect(28., 28., 800., 600.)))
            )
        );
        // A primary with a negative origin: the cascade is from its corner.
        assert_eq!(
            options_for(&placement, None, &displays(), Some(2)),
            (
                Some(DisplayId::new(2)),
                Some(WindowBounds::Windowed(rect(-1572., 28., 800., 600.)))
            )
        );
    }

    #[test]
    fn a_restored_maximized_window_is_maximized() {
        let frame = rect(-1500., 100., 800., 600.);
        let placement = Placement::Restored {
            display: Some(2),
            bounds: Some(frame),
            maximized: true,
        };
        assert_eq!(
            options_for(&placement, None, &displays(), Some(1)),
            (
                Some(DisplayId::new(2)),
                Some(WindowBounds::Maximized(frame))
            )
        );
    }

    #[test]
    fn a_restored_window_with_no_display_keeps_its_bounds() {
        let frame = rect(300., 200., 1000., 700.);
        let placement = Placement::Restored {
            display: None,
            bounds: Some(frame),
            maximized: false,
        };
        assert_eq!(
            options_for(&placement, None, &displays(), Some(1)),
            (None, Some(WindowBounds::Windowed(frame)))
        );
    }

    #[test]
    fn default_placement_asks_for_nothing() {
        assert_eq!(
            options_for(
                &Placement::Default,
                Some((rect(0., 0., 1., 1.), Some(1))),
                &displays(),
                Some(1)
            ),
            (None, None)
        );
    }
}
