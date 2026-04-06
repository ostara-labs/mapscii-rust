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
            if i == 0 {
                if ring.len() < 3 {
                    return false;
                }
            } else {
                if ring.len() < 3 {
                    continue;
                }
                holes.push(vertices.len() / 2);
            }
            for p in ring {
                vertices.push(p.x);
                vertices.push(p.y);
            }
        }

        let triangles = match earcutr::earcut(&vertices, &holes, 2) {
            Ok(t) => t,
            Err(_) => return false,
        };

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
        true
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
    ) {
        let w = (width - 1.0).max(0.0);

        // Thin line: basic Bresenham
        if w <= 0.0 {
            self.bresenham_line(x0, y0, x1, y1, color);
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
            self.buffer.set_pixel(x0, y0, color);

            let mut e2 = err;
            let x2 = x0;

            if 2 * e2 >= -dx {
                e2 += dy;
                let mut y2 = y0;
                while (e2 as f64) < ed * half_w && (y1 != y2 || dx > dy) {
                    y2 += sy;
                    self.buffer.set_pixel(x0, y2, color);
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
                    self.buffer.set_pixel(x2_inner, y0, color);
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

    /// Basic Bresenham line (1px wide).
    fn bresenham_line(&mut self, mut x0: i32, mut y0: i32, x1: i32, y1: i32, color: ColorIdx) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx: i32 = if x0 < x1 { 1 } else { -1 };
        let sy: i32 = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;

        loop {
            self.buffer.set_pixel(x0, y0, color);
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

    /// Return all points on a Bresenham line as `(x, y)` pairs.
    fn bresenham_points(ax: i32, ay: i32, bx: i32, by: i32) -> Vec<(i32, i32)> {
        let mut pts = Vec::new();
        let dx = (bx - ax).abs();
        let dy = -(by - ay).abs();
        let sx: i32 = if ax < bx { 1 } else { -1 };
        let sy: i32 = if ay < by { 1 } else { -1 };
        let mut err = dx + dy;
        let mut x = ax;
        let mut y = ay;

        loop {
            pts.push((x, y));
            if x == bx && y == by {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                if x == bx {
                    break;
                }
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                if y == by {
                    break;
                }
                err += dx;
                y += sy;
            }
        }
        pts
    }

    /// Rasterise a filled triangle.
    fn filled_triangle(&mut self, a: [f64; 2], b: [f64; 2], c: [f64; 2], color: ColorIdx) {
        let ai = [a[0] as i32, a[1] as i32];
        let bi = [b[0] as i32, b[1] as i32];
        let ci = [c[0] as i32, c[1] as i32];

        let mut edge_a = Self::bresenham_points(bi[0], bi[1], ci[0], ci[1]);
        let edge_b = Self::bresenham_points(ai[0], ai[1], ci[0], ci[1]);
        let edge_c = Self::bresenham_points(ai[0], ai[1], bi[0], bi[1]);

        edge_a.extend_from_slice(&edge_b);
        edge_a.extend_from_slice(&edge_c);

        // Filter to visible rows and sort by (y, x)
        let height = self.height as i32;
        edge_a.retain(|&(_, y)| y >= 0 && y < height);
        edge_a.sort_by(|a, b| {
            if a.1 == b.1 {
                a.0.cmp(&b.0)
            } else {
                a.1.cmp(&b.1)
            }
        });

        let mut i = 0;
        while i < edge_a.len() {
            let (px, py) = edge_a[i];
            let next = edge_a.get(i + 1);

            if let Some(&(nx, ny)) = next {
                if py == ny {
                    // Fill horizontal span
                    let left = px.max(0);
                    let right = nx.min(self.width as i32 - 1);
                    if left >= 0 && right < self.width as i32 {
                        for x in left..=right {
                            self.buffer.set_pixel(x, py, color);
                        }
                    }
                } else {
                    self.buffer.set_pixel(px, py, color);
                }
            } else {
                self.buffer.set_pixel(px, py, color);
                break;
            }

            i += 1;
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
        let ai = [a[0] as i32, a[1] as i32];
        let bi = [b[0] as i32, b[1] as i32];
        let ci = [c[0] as i32, c[1] as i32];

        let mut edge_a = Self::bresenham_points(bi[0], bi[1], ci[0], ci[1]);
        let edge_b = Self::bresenham_points(ai[0], ai[1], ci[0], ci[1]);
        let edge_c = Self::bresenham_points(ai[0], ai[1], bi[0], bi[1]);

        edge_a.extend_from_slice(&edge_b);
        edge_a.extend_from_slice(&edge_c);

        let height = self.height as i32;
        edge_a.retain(|&(_, y)| y >= 0 && y < height);
        edge_a.sort_by(|a, b| {
            if a.1 == b.1 {
                a.0.cmp(&b.0)
            } else {
                a.1.cmp(&b.1)
            }
        });

        let mut i = 0;
        while i < edge_a.len() {
            let (px, py) = edge_a[i];
            let next = edge_a.get(i + 1);

            if let Some(&(nx, ny)) = next {
                if py == ny {
                    let left = px.max(0);
                    let right = nx.min(self.width as i32 - 1);
                    if left >= 0 && right < self.width as i32 {
                        for x in left..=right {
                            let threshold = Self::BAYER4X4[(py as usize) & 3][(x as usize) & 3];
                            if opacity > threshold {
                                self.buffer.set_pixel(x, py, color);
                            }
                        }
                    }
                } else {
                    let threshold = Self::BAYER4X4[(py as usize) & 3][(px as usize) & 3];
                    if opacity > threshold {
                        self.buffer.set_pixel(px, py, color);
                    }
                }
            } else {
                let threshold = Self::BAYER4X4[(py as usize) & 3][(px as usize) & 3];
                if opacity > threshold {
                    self.buffer.set_pixel(px, py, color);
                }
                break;
            }

            i += 1;
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
    fn test_bresenham_horizontal() {
        let pts = Canvas::bresenham_points(0, 0, 5, 0);
        assert_eq!(pts.len(), 6);
        assert_eq!(pts[0], (0, 0));
        assert_eq!(pts[5], (5, 0));
    }

    #[test]
    fn test_bresenham_vertical() {
        let pts = Canvas::bresenham_points(0, 0, 0, 5);
        assert_eq!(pts.len(), 6);
        assert_eq!(pts[0], (0, 0));
        assert_eq!(pts[5], (0, 5));
    }

    #[test]
    fn test_bresenham_diagonal() {
        let pts = Canvas::bresenham_points(0, 0, 3, 3);
        assert_eq!(pts.len(), 4);
        for (i, &(x, y)) in pts.iter().enumerate() {
            assert_eq!(x, i as i32);
            assert_eq!(y, i as i32);
        }
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
