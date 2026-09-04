//! Geo-math utilities, color conversion, and helper functions.
//!
//! Direct translation of the original mapscii `utils.js`.

use crate::config::MapConfig;

const EARTH_RADIUS: f64 = 6378137.0;

/// Clamp `num` to `[min, max]`.
#[inline]
pub fn clamp(num: f64, min: f64, max: f64) -> f64 {
    num.clamp(min, max)
}

/// Compute the integer tile zoom used for fetching tiles, clamped to `[0, tile_range]`.
///
/// Uses `floor()` so that z2.x always fetches z2 tiles, z3.x fetches z3 tiles,
/// etc. The tile data only changes at integer boundaries — within a given
/// integer band the renderer just scales the same tile polygons up via
/// `tilesize_at_zoom()`. This keeps coastline shapes stable across an entire
/// zoom band and avoids mid-band data cliffs.
///
/// **Terminology**:
/// - *display zoom* (`zoom: f64`): the fractional zoom the user sees (e.g. 2.7).
/// - *tile zoom* (return value): the integer zoom used to address tiles (e.g. 2).
#[inline]
pub fn base_zoom(zoom: f64, config: &MapConfig) -> u8 {
    let z = zoom.floor() as i32;
    z.clamp(0, config.tile_range as i32) as u8
}

/// The effective tile pixel size at the given fractional zoom.
#[inline]
pub fn tilesize_at_zoom(zoom: f64, config: &MapConfig) -> f64 {
    let bz = base_zoom(zoom, config) as f64;
    config.project_size as f64 * 2.0_f64.powf(zoom - bz)
}

/// Convert degrees to radians.
#[inline]
pub fn deg2rad(angle: f64) -> f64 {
    angle * std::f64::consts::PI / 180.0
}

/// A tile coordinate (fractional x, y at a given zoom).
#[derive(Debug, Clone, Copy)]
pub struct TileCoord {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Convert longitude/latitude to fractional tile coordinates at the given zoom.
pub fn ll2tile(lon: f64, lat: f64, zoom: f64) -> TileCoord {
    let n = 2.0_f64.powf(zoom);
    let lat_rad = lat * std::f64::consts::PI / 180.0;
    TileCoord {
        x: (lon + 180.0) / 360.0 * n,
        y: (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n,
        z: zoom,
    }
}

/// Convert tile coordinates back to longitude/latitude.
pub fn tile2ll(x: f64, y: f64, zoom: f64) -> (f64, f64) {
    let n = std::f64::consts::PI - 2.0 * std::f64::consts::PI * y / 2.0_f64.powf(zoom);
    let lon = x / 2.0_f64.powf(zoom) * 360.0 - 180.0;
    let lat = 180.0 / std::f64::consts::PI * (0.5 * (n.exp() - (-n).exp())).atan();
    (lon, lat)
}

/// Meters per pixel at the given zoom and latitude.
pub fn meters_per_pixel(zoom: f64, lat: f64) -> f64 {
    let lat_rad = lat * std::f64::consts::PI / 180.0;
    (lat_rad.cos() * 2.0 * std::f64::consts::PI * EARTH_RADIUS) / (256.0 * 2.0_f64.powf(zoom))
}

/// Parse a CSS hex color string (`#rgb` or `#rrggbb`) into `[r, g, b]`.
///
/// Returns `[255, 0, 0]` (red) for invalid input, matching the original JS behavior.
pub fn hex2rgb(color: &str) -> [u8; 3] {
    let color = color.trim();
    if !color.starts_with('#') {
        return [255, 0, 0];
    }

    let hex = &color[1..];
    if hex.len() == 3 {
        let Ok(decimal) = u32::from_str_radix(hex, 16) else {
            return [255, 0, 0];
        };
        let r = ((decimal >> 8) & 0xF) as u8;
        let g = ((decimal >> 4) & 0xF) as u8;
        let b = (decimal & 0xF) as u8;
        [r | (r << 4), g | (g << 4), b | (b << 4)]
    } else if hex.len() == 6 {
        let Ok(decimal) = u32::from_str_radix(hex, 16) else {
            return [255, 0, 0];
        };
        [
            ((decimal >> 16) & 0xFF) as u8,
            ((decimal >> 8) & 0xFF) as u8,
            (decimal & 0xFF) as u8,
        ]
    } else {
        [255, 0, 0]
    }
}

/// Truncate `number` to `digits` decimal places.
pub fn digits(number: f64, precision: u32) -> f64 {
    let factor = 10.0_f64.powi(precision as i32);
    (number * factor).floor() / factor
}

/// Normalize a lon/lat pair: wrap longitude to `[-180, 180]`, clamp latitude to
/// the Web Mercator limit of `[-85.0511, 85.0511]`.
pub fn normalize(lon: &mut f64, lat: &mut f64) {
    if *lon < -180.0 {
        *lon += 360.0;
    }
    if *lon > 180.0 {
        *lon -= 360.0;
    }
    *lat = (*lat).clamp(-85.0511, 85.0511);
}

/// Population count (number of set bits) of a `u32`.
#[inline]
pub fn population(val: u32) -> u32 {
    val.count_ones()
}

/// Simplify a polyline using the Ramer-Douglas-Peucker algorithm.
///
/// `points` is a slice of `(x, y)` pairs; `epsilon` is the distance threshold.
pub fn simplify(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if points.len() < 3 {
        return points.to_vec();
    }

    // Find the point with maximum distance from the line between first and last
    let (first, last) = (points[0], points[points.len() - 1]);
    let mut max_dist = 0.0_f64;
    let mut max_idx = 0;

    for (i, &p) in points.iter().enumerate().skip(1).take(points.len() - 2) {
        let d = perpendicular_distance(p, first, last);
        if d > max_dist {
            max_dist = d;
            max_idx = i;
        }
    }

    if max_dist > epsilon {
        let mut left = simplify(&points[..=max_idx], epsilon);
        let right = simplify(&points[max_idx..], epsilon);
        left.pop(); // remove duplicate point at the split
        left.extend_from_slice(&right);
        left
    } else {
        vec![first, last]
    }
}

/// Perpendicular distance from point `p` to the line defined by `a` and `b`.
fn perpendicular_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let denom = (dx * dx + dy * dy).sqrt();
    if denom == 0.0 {
        let ex = p.0 - a.0;
        let ey = p.1 - a.1;
        return (ex * ex + ey * ey).sqrt();
    }
    ((dy * p.0 - dx * p.1 + b.0 * a.1 - b.1 * a.0) / denom).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex2rgb_six_digit() {
        assert_eq!(hex2rgb("#ff0000"), [255, 0, 0]);
        assert_eq!(hex2rgb("#00ff00"), [0, 255, 0]);
        assert_eq!(hex2rgb("#5f87ff"), [95, 135, 255]);
    }

    #[test]
    fn test_hex2rgb_three_digit() {
        assert_eq!(hex2rgb("#fff"), [255, 255, 255]);
        assert_eq!(hex2rgb("#000"), [0, 0, 0]);
        assert_eq!(hex2rgb("#f00"), [255, 0, 0]);
        assert_eq!(hex2rgb("#9bf"), [0x99, 0xBB, 0xFF]);
    }

    #[test]
    fn test_hex2rgb_invalid() {
        assert_eq!(hex2rgb("not-a-color"), [255, 0, 0]);
    }

    #[test]
    fn test_ll2tile_roundtrip() {
        let lon = 13.42012;
        let lat = 52.51298;
        let zoom = 10.0;
        let tc = ll2tile(lon, lat, zoom);
        let (rlon, rlat) = tile2ll(tc.x, tc.y, zoom);
        assert!((rlon - lon).abs() < 0.001);
        assert!((rlat - lat).abs() < 0.001);
    }

    #[test]
    fn test_normalize() {
        let mut lon = -200.0;
        let mut lat = 90.0;
        normalize(&mut lon, &mut lat);
        assert_eq!(lon, 160.0);
        assert_eq!(lat, 85.0511);
    }

    #[test]
    fn test_population() {
        assert_eq!(population(0), 0);
        assert_eq!(population(0b1010), 2);
        assert_eq!(population(0xFF), 8);
    }

    #[test]
    fn test_simplify_trivial() {
        let pts = vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)];
        let result = simplify(&pts, 0.1);
        assert_eq!(result, vec![(0.0, 0.0), (2.0, 0.0)]);
    }

    #[test]
    #[allow(clippy::approx_constant)] // 3.14 is the expected truncation of PI — using PI for both operands would be a tautology
    fn test_digits() {
        assert!((digits(std::f64::consts::PI, 2) - 3.14).abs() < 1e-10);
    }
}
