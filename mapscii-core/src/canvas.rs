//! Canvas-like painting abstraction over `BrailleBuffer`.
//!
//! Provides Bresenham line drawing, thick lines (Zingl's algorithm),
//! earcut-based polygon triangulation + filled triangle rasterisation,
//! and text rendering.
//!
//! Translated from the original mapscii `Canvas.js`.

use crate::braille_buffer::{BrailleBuffer, ColorIdx};

/// A 2-D point in pixel coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Canvas wraps a `BrailleBuffer` and exposes high-level drawing primitives.
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub buffer: BrailleBuffer,
}

impl Canvas {
    /// Create a new canvas with the given pixel dimensions.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            buffer: BrailleBuffer::new(width, height),
        }
    }

    /// Clear the canvas.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Draw a text label at pixel `(x, y)`.
    pub fn text(&mut self, text: &str, x: i32, y: i32, color: ColorIdx, center: bool) {
        self.buffer.write_text(text, x, y, color, center);
    }

    /// Draw a line between two points.
    pub fn line(&mut self, from: Point, to: Point, color: ColorIdx, width: f64) {
        self.draw_line(
            from.x as i32,
            from.y as i32,
            to.x as i32,
            to.y as i32,
            width,
            color,
            false,
        );
    }

    /// Draw a polyline (connected line segments).
    pub fn polyline(&mut self, points: &[Point], color: ColorIdx, width: f64) {
        for pair in points.windows(2) {
            self.draw_line(
                pair[0].x as i32,
                pair[0].y as i32,
                pair[1].x as i32,
                pair[1].y as i32,
                width,
                color,
                false,
            );
        }
    }

    /// Draw a polyline that renders as single braille dots per cell, preventing
    /// filled backgrounds (e.g. water) from making the line appear thick.
    pub fn polyline_thin(&mut self, points: &[Point], color: ColorIdx, width: f64) {
        for pair in points.windows(2) {
            self.draw_line(
                pair[0].x as i32,
                pair[0].y as i32,
                pair[1].x as i32,
                pair[1].y as i32,
                width,
                color,
                true,
            );
        }
    }

    /// Set the global background color.
    pub fn set_background(&mut self, color: ColorIdx) {
        self.buffer.set_global_background(color);
    }

    /// Set the background color for a specific pixel cell.
    pub fn background(&mut self, x: i32, y: i32, color: ColorIdx) {
        self.buffer.set_background(x, y, color);
    }

    /// Fill a polygon defined by rings (outer ring + optional hole rings).
    ///
    /// Each ring is a slice of `Point`s. The first ring is the exterior,
    /// subsequent rings are holes. Returns `true` if triangulation succeeded.
    pub fn polygon(&mut self, rings: &[Vec<Point>], color: ColorIdx) -> bool {
        self.polygon_fill(rings, color, 1.0)
    }

    /// Fill a polygon with opacity support (ordered dithering).
    ///
    /// `opacity` controls pixel density: 1.0 = solid fill, 0.0 = no pixels drawn.
    /// Uses a 4×4 Bayer matrix for visually uniform dithering.
    pub fn polygon_fill(&mut self, rings: &[Vec<Point>], color: ColorIdx, opacity: f64) -> bool {
        if opacity <= 0.0 {
            return true;
        }

        let mut vertices: Vec<f64> = Vec::new();
        let mut holes: Vec<usize> = Vec::new();

        for (i, ring) in rings.iter().enumerate() {
            let opened = Self::open_ring(ring);
            let pts = Self::remove_collinear(opened);
            if i == 0 {
                if pts.len() < 3 {
                    return false;
                }
            } else {
                if pts.len() < 3 {
                    continue;
                }
                holes.push(vertices.len() / 2);
            }
            for p in &pts {
                vertices.push(p.x);
                vertices.push(p.y);
            }
        }

        let vert_count = vertices.len() / 2;
        let triangles = match earcutr::earcut(&vertices, &holes, 2) {
            Ok(t) if !t.is_empty() => t,
            result => {
                if !holes.is_empty() {
                    let opened = Self::open_ring(&rings[0]);
                    let cleaned = Self::remove_collinear(opened);
                    let outer_verts: Vec<f64> = cleaned.iter().flat_map(|p| [p.x, p.y]).collect();
                    match earcutr::earcut(&outer_verts, &[], 2) {
                        Ok(t) if !t.is_empty() => {
                            log::debug!(
                                "[canvas] earcut retry without holes: verts={} tris={}",
                                cleaned.len(),
                                t.len() / 3
                            );
                            self.rasterize_triangles(&outer_verts, &t, color, opacity);
                            return true;
                        }
                        _ => {}
                    }
                }
                if let Err(e) = result {
                    log::debug!(
                        "[canvas] earcut failed: {e:?} verts={vert_count} holes={}",
                        holes.len()
                    );
                }
                return false;
            }
        };
        let tri_count = triangles.len() / 3;

        self.rasterize_triangles(&vertices, &triangles, color, opacity);

        if tri_count > 500 {
            log::debug!(
                "[canvas] large polygon verts={vert_count} holes={} tris={tri_count}",
                holes.len(),
            );
        }
        true
    }

    /// Strip the closing duplicate from a ring (last point == first point).
    fn open_ring(ring: &[Point]) -> &[Point] {
        if ring.len() >= 2 {
            let first = &ring[0];
            let last = &ring[ring.len() - 1];
            if first.x == last.x && first.y == last.y {
                return &ring[..ring.len() - 1];
            }
        }
        ring
    }

    /// Remove strictly collinear middle points from an open ring.
    /// Three consecutive points A-B-C are collinear when the cross product
    /// `(B-A) × (C-A)` is zero (integer coords → exact test).
    fn remove_collinear(pts: &[Point]) -> Vec<Point> {
        if pts.len() <= 3 {
            return pts.to_vec();
        }
        let mut out: Vec<Point> = Vec::with_capacity(pts.len());
        let n = pts.len();
        for i in 0..n {
            let a = &pts[(i + n - 1) % n];
            let b = &pts[i];
            let c = &pts[(i + 1) % n];
            let cross = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
            if cross != 0.0 {
                out.push(*b);
            }
        }
        if out.len() < 3 {
            return pts.to_vec();
        }
        out
    }

    fn rasterize_triangles(
        &mut self,
        vertices: &[f64],
        triangles: &[usize],
        color: ColorIdx,
        opacity: f64,
    ) {
        if opacity >= 1.0 {
            for tri in triangles.chunks_exact(3) {
                let pa = [vertices[tri[0] * 2], vertices[tri[0] * 2 + 1]];
                let pb = [vertices[tri[1] * 2], vertices[tri[1] * 2 + 1]];
                let pc = [vertices[tri[2] * 2], vertices[tri[2] * 2 + 1]];
                self.filled_triangle(pa, pb, pc, color);
            }
        } else {
            for tri in triangles.chunks_exact(3) {
                let pa = [vertices[tri[0] * 2], vertices[tri[0] * 2 + 1]];
                let pb = [vertices[tri[1] * 2], vertices[tri[1] * 2 + 1]];
                let pc = [vertices[tri[2] * 2], vertices[tri[2] * 2 + 1]];
                self.dithered_triangle(pa, pb, pc, color, opacity);
            }
        }
    }

    /// Draw only the outline (stroke) of a polygon.
    pub fn polygon_stroke(&mut self, rings: &[Vec<Point>], color: ColorIdx, width: f64) {
        for ring in rings {
            if ring.len() >= 2 {
                self.polyline(ring, color, width);
                if let (Some(first), Some(last)) = (ring.first(), ring.last()) {
                    self.line(*last, *first, color, width);
                }
            }
        }
    }

    // -- Private drawing helpers -------------------------------------------

    /// Bresenham line with optional width (Zingl's algorithm).
    fn draw_line(
        &mut self,
        mut x0: i32,
        mut y0: i32,
        x1: i32,
        y1: i32,
        width: f64,
        color: ColorIdx,
        thin: bool,
    ) {
        let w = (width - 1.0).max(0.0);

        if w <= 0.0 {
            self.bresenham_line(x0, y0, x1, y1, color, thin);
            return;
        }

        let dx = (x1 - x0).abs();
        let sx: i32 = if x0 < x1 { 1 } else { -1 };
        let dy = (y1 - y0).abs();
        let sy: i32 = if y0 < y1 { 1 } else { -1 };

        let mut err = dx - dy;
        let ed = if dx + dy == 0 {
            1.0
        } else {
            ((dx * dx + dy * dy) as f64).sqrt()
        };
        let half_w = (w + 1.0) / 2.0;

        loop {
            if thin {
                self.buffer.set_pixel_line(x0, y0, color);
            } else {
                self.buffer.set_pixel(x0, y0, color);
            }

            let mut e2 = err;
            let x2 = x0;

            if 2 * e2 >= -dx {
                e2 += dy;
                let mut y2 = y0;
                while (e2 as f64) < ed * half_w && (y1 != y2 || dx > dy) {
                    y2 += sy;
                    if thin {
                        self.buffer.set_pixel_line(x0, y2, color);
                    } else {
                        self.buffer.set_pixel(x0, y2, color);
                    }
                    e2 += dx;
                }
                if x0 == x1 {
                    break;
                }
                e2 = err;
                err -= dy;
                x0 += sx;
            }

            if 2 * e2 <= dy {
                e2 = dx - e2;
                let mut x2_inner = x2;
                while (e2 as f64) < ed * half_w && (x1 != x2_inner || dx < dy) {
                    x2_inner += sx;
                    if thin {
                        self.buffer.set_pixel_line(x2_inner, y0, color);
                    } else {
                        self.buffer.set_pixel(x2_inner, y0, color);
                    }
                    e2 += dy;
                }
                if y0 == y1 {
                    break;
                }
                err += dx;
                y0 += sy;
            }
        }
    }

    fn bresenham_line(
        &mut self,
        mut x0: i32,
        mut y0: i32,
        x1: i32,
        y1: i32,
        color: ColorIdx,
        thin: bool,
    ) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx: i32 = if x0 < x1 { 1 } else { -1 };
        let sy: i32 = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;

        loop {
            if thin {
                self.buffer.set_pixel_line(x0, y0, color);
            } else {
                self.buffer.set_pixel(x0, y0, color);
            }
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                if x0 == x1 {
                    break;
                }
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                if y0 == y1 {
                    break;
                }
                err += dx;
                y0 += sy;
            }
        }
    }

    /// Rasterise a filled triangle using scanline edge intersection.
    fn filled_triangle(&mut self, a: [f64; 2], b: [f64; 2], c: [f64; 2], color: ColorIdx) {
        let w = self.width as i32;
        let h = self.height as i32;

        let min_y = (a[1].min(b[1]).min(c[1]).ceil() as i32).max(0);
        let max_y = (a[1].max(b[1]).max(c[1]).floor() as i32).min(h - 1);
        if min_y > max_y {
            return;
        }

        let edges: [[f64; 2]; 3] = [a, b, c];

        for y in min_y..=max_y {
            let yf = y as f64 + 0.5;
            let (mut x_min, mut x_max) = (f64::MAX, f64::MIN);

            for i in 0..3 {
                let p0 = edges[i];
                let p1 = edges[(i + 1) % 3];
                let (y0, y1) = (p0[1], p1[1]);
                if (y0 <= yf && y1 > yf) || (y1 <= yf && y0 > yf) {
                    let t = (yf - y0) / (y1 - y0);
                    let x = p0[0] + t * (p1[0] - p0[0]);
                    x_min = x_min.min(x);
                    x_max = x_max.max(x);
                }
            }

            if x_min > x_max {
                continue;
            }

            let left = (x_min.ceil() as i32).max(0);
            let right = (x_max.floor() as i32).min(w - 1);
            for x in left..=right {
                self.buffer.set_pixel(x, y, color);
            }
        }
    }

    /// Scanline even-odd polygon fill on integer-snapped rings, bypassing
    /// earcut triangulation. Processes all rings (outer + holes) together
    /// using even-odd parity for correct hole handling.
    pub fn scanline_polygon_fill(&mut self, rings: &[Vec<Point>], color: ColorIdx) {
        let w = self.width as i32;
        let h = self.height as i32;

        let mut edges: Vec<([i32; 2], [i32; 2])> = Vec::new();
        for ring in rings {
            let pts = Self::open_ring(ring);
            if pts.len() < 2 {
                continue;
            }
            for i in 0..pts.len() {
                let a = [pts[i].x as i32, pts[i].y as i32];
                let b = [
                    pts[(i + 1) % pts.len()].x as i32,
                    pts[(i + 1) % pts.len()].y as i32,
                ];
                if a[1] != b[1] {
                    edges.push((a, b));
                }
            }
        }

        if edges.is_empty() {
            return;
        }

        let mut min_y = i32::MAX;
        let mut max_y = i32::MIN;
        for &(a, b) in &edges {
            min_y = min_y.min(a[1]).min(b[1]);
            max_y = max_y.max(a[1]).max(b[1]);
        }
        min_y = min_y.max(0);
        max_y = max_y.min(h - 1);

        let mut x_intersections: Vec<i32> = Vec::new();

        for y in min_y..=max_y {
            x_intersections.clear();
            let yf = y as f64 + 0.5;

            for &(a, b) in &edges {
                let (y0, y1) = (a[1] as f64, b[1] as f64);
                if (y0 <= yf && y1 > yf) || (y1 <= yf && y0 > yf) {
                    let t = (yf - y0) / (y1 - y0);
                    let x = a[0] as f64 + t * (b[0] - a[0]) as f64;
                    x_intersections.push(x.round() as i32);
                }
            }

            x_intersections.sort_unstable();

            for pair in x_intersections.chunks_exact(2) {
                let left = pair[0].max(0);
                let right = pair[1].min(w - 1);
                for x in left..=right {
                    self.buffer.set_pixel(x, y, color);
                }
            }
        }
    }

    // 4×4 Bayer ordered-dithering threshold matrix (normalized to 0.0–1.0).
    const BAYER4X4: [[f64; 4]; 4] = [
        [0.0 / 16.0, 8.0 / 16.0, 2.0 / 16.0, 10.0 / 16.0],
        [12.0 / 16.0, 4.0 / 16.0, 14.0 / 16.0, 6.0 / 16.0],
        [3.0 / 16.0, 11.0 / 16.0, 1.0 / 16.0, 9.0 / 16.0],
        [15.0 / 16.0, 7.0 / 16.0, 13.0 / 16.0, 5.0 / 16.0],
    ];

    fn dithered_triangle(
        &mut self,
        a: [f64; 2],
        b: [f64; 2],
        c: [f64; 2],
        color: ColorIdx,
        opacity: f64,
    ) {
        let w = self.width as i32;
        let h = self.height as i32;

        let min_y = (a[1].min(b[1]).min(c[1]).ceil() as i32).max(0);
        let max_y = (a[1].max(b[1]).max(c[1]).floor() as i32).min(h - 1);
        if min_y > max_y {
            return;
        }

        let edges: [[f64; 2]; 3] = [a, b, c];

        for y in min_y..=max_y {
            let yf = y as f64 + 0.5;
            let (mut x_min, mut x_max) = (f64::MAX, f64::MIN);

            for i in 0..3 {
                let p0 = edges[i];
                let p1 = edges[(i + 1) % 3];
                let (y0, y1) = (p0[1], p1[1]);
                if (y0 <= yf && y1 > yf) || (y1 <= yf && y0 > yf) {
                    let t = (yf - y0) / (y1 - y0);
                    let x = p0[0] + t * (p1[0] - p0[0]);
                    x_min = x_min.min(x);
                    x_max = x_max.max(x);
                }
            }

            if x_min > x_max {
                continue;
            }

            let left = (x_min.ceil() as i32).max(0);
            let right = (x_max.floor() as i32).min(w - 1);
            for x in left..=right {
                let threshold = Self::BAYER4X4[(y as usize) & 3][(x as usize) & 3];
                if opacity > threshold {
                    self.buffer.set_pixel(x, y, color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canvas_creation() {
        let c = Canvas::new(80, 40);
        assert_eq!(c.width, 80);
        assert_eq!(c.height, 40);
        assert_eq!(c.buffer.cols(), 40);
        assert_eq!(c.buffer.rows(), 10);
    }

    #[test]
    fn test_polyline() {
        let mut c = Canvas::new(20, 8);
        let points = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 4.0),
        ];
        c.polyline(&points, 1, 1.0);
        // Just verify it doesn't panic
    }

    #[test]
    fn test_polygon_simple_triangle() {
        let mut c = Canvas::new(20, 8);
        let ring = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(5.0, 7.0),
        ];
        let result = c.polygon(&[ring], 1);
        assert!(result);
    }

    #[test]
    fn test_polygon_too_few_points() {
        let mut c = Canvas::new(20, 8);
        let ring = vec![Point::new(0.0, 0.0), Point::new(10.0, 0.0)];
        let result = c.polygon(&[ring], 1);
        assert!(!result);
    }
}
