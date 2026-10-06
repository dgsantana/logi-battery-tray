//! Battery tray icon drawn as RGBA pixels, for platforms without a themed
//! battery icon set (Windows). Plain Rust so it is tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use crate::hidpp::{BatteryStatus, ChargingState};
use crate::state::THRESHOLDS;
use crate::tray;

/// Icon edge in pixels.
pub const SIZE: u32 = 32;

const OUTLINE: [u8; 3] = [0xF0, 0xF0, 0xF0];
/// Outer edge of the outline, so the light outline also shows on light taskbars.
const OUTLINE_DARK: [u8; 3] = [0x30, 0x30, 0x30];
const FILL_OK: [u8; 3] = [0x3C, 0xC8, 0x50];
const FILL_LOW: [u8; 3] = [0xE0, 0x40, 0x40];
const BOLT: [u8; 3] = [0xFF, 0xD7, 0x00];
const MISSING: [u8; 3] = [0xE8, 0xE8, 0xE8];
/// Dimmed interior so the empty part shows on light and dark taskbars.
const INTERIOR: [u8; 4] = [0x00, 0x00, 0x00, 0x60];

/// Body (x0, y0, x1, y1), end-exclusive; the terminal nub sits on top.
const BODY: (u32, u32, u32, u32) = (7, 5, 25, 31);
const NUB: (u32, u32, u32, u32) = (12, 2, 20, 5);
/// Outline thickness: one dark edge pixel plus two light ones.
const BORDER: u32 = 3;
/// Charging bolt outline, in pixel coordinates.
const BOLT_SHAPE: [(f32, f32); 6] = [(17.0, 8.0), (10.0, 19.0), (15.0, 19.0), (13.0, 28.0), (22.0, 16.0), (17.0, 16.0)];

fn inside((x0, y0, x1, y1): (u32, u32, u32, u32), x: u32, y: u32) -> bool {
    (x0..x1).contains(&x) && (y0..y1).contains(&y)
}

/// Whether a pixel belongs to the battery drawing at all.
fn in_shape(x: u32, y: u32) -> bool {
    inside(BODY, x, y) || inside(NUB, x, y)
}

/// Shape pixels with a 4-neighbour outside the shape.
fn on_edge(x: u32, y: u32) -> bool {
    let outside = |dx: i32, dy: i32| match (x.checked_add_signed(dx), y.checked_add_signed(dy)) {
        (Some(nx), Some(ny)) => !in_shape(nx, ny),
        _ => true,
    };
    outside(-1, 0) || outside(1, 0) || outside(0, -1) || outside(0, 1)
}

fn interior() -> (u32, u32, u32, u32) {
    let (x0, y0, x1, y1) = BODY;
    (x0 + BORDER, y0 + BORDER, x1 - BORDER, y1 - BORDER)
}

/// Even-odd point-in-polygon test at the pixel's centre.
fn in_bolt(x: u32, y: u32) -> bool {
    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
    let prev = BOLT_SHAPE.iter().cycle().skip(BOLT_SHAPE.len() - 1);
    let crossings = BOLT_SHAPE
        .iter()
        .zip(prev)
        .filter(|&(&(xi, yi), &(xj, yj))| (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi)
        .count();
    crossings % 2 == 1
}

/// Two diagonals across the interior, for "no reading".
fn in_cross(x: u32, y: u32) -> bool {
    let (x0, y0, x1, y1) = interior();
    let (u, v) = ((x - x0) as f32 / (x1 - x0) as f32, (y - y0) as f32 / (y1 - y0) as f32);
    let w = 1.5 / (x1 - x0) as f32;
    (u - v).abs() < w || (u + v - 1.0).abs() < w
}

/// Icon for the lowest battery: fill by level (red when low), bolt when
/// charging, a cross when there is no reading.
pub fn render(lowest: Option<BatteryStatus>) -> Vec<u8> {
    let (_, iy0, _, iy1) = interior();
    // Any charge left shows at least one row, so 1-9% is not drawn empty.
    let fill_rows = lowest.map_or(0, |b| ((iy1 - iy0) * u32::from(tray::level(b)) / 100).max(u32::from(b.percent > 0)));
    let fill_color = match lowest {
        Some(b) if b.percent <= THRESHOLDS[0] => FILL_LOW,
        _ => FILL_OK,
    };
    let charging = matches!(lowest, Some(b) if matches!(b.charging, ChargingState::Charging | ChargingState::Full));
    let mut px = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            if !in_shape(x, y) {
                continue;
            }
            let rgba = if on_edge(x, y) {
                opaque(OUTLINE_DARK)
            } else if !inside(interior(), x, y) {
                opaque(OUTLINE)
            } else if charging && in_bolt(x, y) {
                opaque(BOLT)
            } else if lowest.is_none() && in_cross(x, y) {
                opaque(MISSING)
            } else if y >= iy1 - fill_rows {
                opaque(fill_color)
            } else {
                INTERIOR
            };
            let at = ((y * SIZE + x) * 4) as usize;
            px[at..at + 4].copy_from_slice(&rgba);
        }
    }
    px
}

fn opaque([r, g, b]: [u8; 3]) -> [u8; 4] {
    [r, g, b, 0xFF]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hidpp::ChargingState;

    fn batt(percent: u8, charging: ChargingState) -> Option<BatteryStatus> {
        Some(BatteryStatus { percent, charging })
    }

    fn count(px: &[u8], rgb: [u8; 3]) -> usize {
        px.chunks(4).filter(|p| p[..3] == rgb && p[3] == 255).count()
    }

    fn fill(px: &[u8]) -> usize {
        count(px, FILL_OK) + count(px, FILL_LOW)
    }

    #[test]
    fn every_icon_is_size_squared_rgba() {
        let inputs = [None, batt(0, ChargingState::Discharging), batt(55, ChargingState::Charging), batt(100, ChargingState::Full)];
        for lowest in inputs {
            assert_eq!(render(lowest).len(), (SIZE * SIZE * 4) as usize);
        }
    }

    #[test]
    fn fill_grows_with_level() {
        let fills: Vec<usize> = (0..=10).map(|i| fill(&render(batt(i * 10, ChargingState::Discharging)))).collect();
        assert_eq!(fills[0], 0);
        assert!(fills.windows(2).all(|w| w[0] < w[1]), "{fills:?}");
    }

    #[test]
    fn low_battery_fills_red_and_healthy_fills_green() {
        let low = render(batt(10, ChargingState::Discharging));
        assert!(count(&low, FILL_LOW) > 0);
        assert_eq!(count(&low, FILL_OK), 0);
        let ok = render(batt(80, ChargingState::Discharging));
        assert!(count(&ok, FILL_OK) > 0);
        assert_eq!(count(&ok, FILL_LOW), 0);
    }

    #[test]
    fn charging_adds_a_bolt() {
        let charging = render(batt(50, ChargingState::Charging));
        assert!(count(&charging, BOLT) > 0);
        assert_ne!(charging, render(batt(50, ChargingState::Discharging)));
        assert!(count(&render(batt(100, ChargingState::Full)), BOLT) > 0);
    }

    #[test]
    fn missing_reading_differs_from_empty_battery() {
        assert_ne!(render(None), render(batt(0, ChargingState::Discharging)));
    }

    #[test]
    fn outside_the_battery_is_transparent() {
        let px = render(batt(100, ChargingState::Charging));
        for y in 0..SIZE {
            for x in 0..SIZE {
                if !in_shape(x, y) {
                    assert_eq!(px[((y * SIZE + x) * 4 + 3) as usize], 0, "pixel {x},{y}");
                }
            }
        }
        assert!(!in_shape(0, 0) && !in_shape(SIZE - 1, 0));
    }

    #[test]
    fn any_charge_left_shows_a_red_sliver() {
        assert!(count(&render(batt(5, ChargingState::Discharging)), FILL_LOW) > 0);
        assert_eq!(fill(&render(batt(0, ChargingState::Discharging))), 0);
    }

    #[test]
    fn outline_has_a_dark_edge_for_light_taskbars() {
        let px = render(batt(50, ChargingState::Discharging));
        assert!(count(&px, OUTLINE_DARK) > 0);
        assert!(count(&px, OUTLINE) > 0);
        // outermost body pixel is the dark edge
        let at = ((BODY.1 + 10) * SIZE + BODY.0) as usize * 4;
        assert_eq!(px[at..at + 3], OUTLINE_DARK);
    }
}
