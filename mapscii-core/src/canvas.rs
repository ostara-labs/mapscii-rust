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

        let clip_min_x = -(CLIP_MARGIN as f64);
        let clip_min_y = -(CLIP_MARGIN as f64);
        let clip_max_x = self.width as f64 + CLIP_MARGIN as f64;
        let clip_max_y = self.height as f64 + CLIP_MARGIN as f64;

        let mut vertices: Vec<f64> = Vec::new();
        let mut holes: Vec<usize> = Vec::new();

        for (i, ring) in rings.iter().enumerate() {
            let clipped = clip_polygon_sutherland_hodgman(
                ring, clip_min_x, clip_min_y, clip_max_x, clip_max_y,
            );
            if i == 0 {
                if clipped.len() < 3 {
                    return false;
                }
            } else {
                if clipped.len() < 3 {
                    continue;
                }
                holes.push(vertices.len() / 2);
            }
            for p in &clipped {
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

    fn fill_background_span(&mut self, left: i32, right: i32, y: i32, color: ColorIdx) {
        let cell_left = (left.max(0) as usize) / 2;
        let cell_right = (right.max(0) as usize) / 2;
        let cell_y_top = (y.max(0) as usize) & !3;
        for cx in cell_left..=cell_right {
            self.buffer
                .set_background(cx as i32 * 2, cell_y_top as i32, color);
        }
    }

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
                            self.buffer.set_pixel(x, py, color);
                        }
                        self.fill_background_span(left, right, py, color);
                    }
                } else {
                    self.buffer.set_pixel(px, py, color);
                    self.buffer.set_background(px, py, color);
                }
            } else {
                self.buffer.set_pixel(px, py, color);
                self.buffer.set_background(px, py, color);
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

const CLIP_MARGIN: usize = 4;

fn clip_polygon_sutherland_hodgman(
    ring: &[Point],
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
) -> Vec<Point> {
    if ring.len() < 3 {
        return Vec::new();
    }

    let mut output: Vec<Point> = ring.to_vec();

    let edges: [(
        fn(&Point, f64) -> bool,
        fn(&Point, &Point, f64) -> Point,
        f64,
    ); 4] = [
        (|p, v| p.x >= v, intersect_left, min_x),
        (|p, v| p.x <= v, intersect_right, max_x),
        (|p, v| p.y >= v, intersect_top, min_y),
        (|p, v| p.y <= v, intersect_bottom, max_y),
    ];

    for &(inside, intersect, val) in &edges {
        if output.is_empty() {
            return output;
        }
        let input = output;
        output = Vec::with_capacity(input.len());

        let mut prev = input[input.len() - 1];
        let mut prev_inside = inside(&prev, val);

        for &curr in &input {
            let curr_inside = inside(&curr, val);
            if curr_inside {
                if !prev_inside {
                    output.push(intersect(&prev, &curr, val));
                }
                output.push(curr);
            } else if prev_inside {
                output.push(intersect(&prev, &curr, val));
            }
            prev = curr;
            prev_inside = curr_inside;
        }
    }

    output
}

fn intersect_left(a: &Point, b: &Point, x: f64) -> Point {
    let t = (x - a.x) / (b.x - a.x);
    Point::new(x, a.y + t * (b.y - a.y))
}

fn intersect_right(a: &Point, b: &Point, x: f64) -> Point {
    intersect_left(a, b, x)
}

fn intersect_top(a: &Point, b: &Point, y: f64) -> Point {
    let t = (y - a.y) / (b.y - a.y);
    Point::new(a.x + t * (b.x - a.x), y)
}

fn intersect_bottom(a: &Point, b: &Point, y: f64) -> Point {
    intersect_top(a, b, y)
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

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn points_approx_eq(a: &[Point], b: &[Point]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|(p, q)| approx_eq(p.x, q.x) && approx_eq(p.y, q.y))
    }

    fn shoelace_area(pts: &[Point]) -> f64 {
        let n = pts.len();
        (0..n)
            .map(|i| {
                let j = (i + 1) % n;
                pts[i].x * pts[j].y - pts[j].x * pts[i].y
            })
            .sum()
    }

    #[test]
    fn test_clip_degenerate_ring() {
        let empty = clip_polygon_sutherland_hodgman(&[], 0.0, 0.0, 100.0, 100.0);
        assert!(empty.is_empty());

        let one = clip_polygon_sutherland_hodgman(&[Point::new(5.0, 5.0)], 0.0, 0.0, 100.0, 100.0);
        assert!(one.is_empty());

        let two = clip_polygon_sutherland_hodgman(
            &[Point::new(5.0, 5.0), Point::new(10.0, 10.0)],
            0.0,
            0.0,
            100.0,
            100.0,
        );
        assert!(two.is_empty());
    }

    #[test]
    fn test_clip_fully_inside() {
        let ring = vec![
            Point::new(10.0, 10.0),
            Point::new(50.0, 10.0),
            Point::new(30.0, 40.0),
        ];
        let clipped = clip_polygon_sutherland_hodgman(&ring, 0.0, 0.0, 100.0, 100.0);
        assert!(points_approx_eq(&clipped, &ring));
    }

    #[test]
    fn test_clip_fully_outside() {
        let ring = vec![
            Point::new(-30.0, 10.0),
            Point::new(-10.0, 10.0),
            Point::new(-20.0, 40.0),
        ];
        let clipped = clip_polygon_sutherland_hodgman(&ring, 0.0, 0.0, 100.0, 100.0);
        assert!(clipped.is_empty());
    }

    #[test]
    fn test_clip_fully_outside_above() {
        let ring = vec![
            Point::new(20.0, -30.0),
            Point::new(50.0, -10.0),
            Point::new(30.0, -20.0),
        ];
        let clipped = clip_polygon_sutherland_hodgman(&ring, 0.0, 0.0, 100.0, 100.0);
        assert!(clipped.is_empty());
    }

    #[test]
    fn test_clip_partial_left_edge() {
        // Given a square straddling the left edge, the left half is clipped
        let ring = vec![
            Point::new(-20.0, 20.0),
            Point::new(40.0, 20.0),
            Point::new(40.0, 60.0),
            Point::new(-20.0, 60.0),
        ];
        let clipped = clip_polygon_sutherland_hodgman(&ring, 0.0, 0.0, 100.0, 100.0);
        let expected = vec![
            Point::new(0.0, 20.0),
            Point::new(40.0, 20.0),
            Point::new(40.0, 60.0),
            Point::new(0.0, 60.0),
        ];
        assert_eq!(clipped.len(), expected.len());
        assert!(points_approx_eq(&clipped, &expected));
    }

    #[test]
    fn test_clip_partial_right_edge() {
        // Given a square straddling the right edge, the right half is clipped
        let ring = vec![
            Point::new(60.0, 20.0),
            Point::new(120.0, 20.0),
            Point::new(120.0, 60.0),
            Point::new(60.0, 60.0),
        ];
        let clipped = clip_polygon_sutherland_hodgman(&ring, 0.0, 0.0, 100.0, 100.0);
        let expected = vec![
            Point::new(60.0, 20.0),
            Point::new(100.0, 20.0),
            Point::new(100.0, 60.0),
            Point::new(60.0, 60.0),
        ];
        assert_eq!(clipped.len(), expected.len());
        assert!(points_approx_eq(&clipped, &expected));
    }

    #[test]
    fn test_clip_corner_produces_rectangle() {
        // Given a square (-20,-20)→(40,40) and clip rect (0,0)→(100,100),
        // then clipping against left and top edges yields (0,0)→(40,40)
        let ring = vec![
            Point::new(-20.0, -20.0),
            Point::new(40.0, -20.0),
            Point::new(40.0, 40.0),
            Point::new(-20.0, 40.0),
        ];
        let clipped = clip_polygon_sutherland_hodgman(&ring, 0.0, 0.0, 100.0, 100.0);
        assert!(clipped.len() >= 3);
        for p in &clipped {
            assert!(p.x >= -1e-9, "x {} should be >= 0", p.x);
            assert!(p.y >= -1e-9, "y {} should be >= 0", p.y);
            assert!(p.x <= 40.0 + 1e-9, "x {} should be <= 40", p.x);
            assert!(p.y <= 40.0 + 1e-9, "y {} should be <= 40", p.y);
        }
    }

    #[test]
    fn test_clip_polygon_covers_viewport() {
        // Given a polygon that fully encloses the clip rect,
        // then the result is a rectangle matching the clip bounds
        let ring = vec![
            Point::new(-500.0, -500.0),
            Point::new(600.0, -500.0),
            Point::new(600.0, 600.0),
            Point::new(-500.0, 600.0),
        ];
        let clipped = clip_polygon_sutherland_hodgman(&ring, 0.0, 0.0, 100.0, 100.0);
        assert_eq!(clipped.len(), 4);
        for p in &clipped {
            assert!(
                (approx_eq(p.x, 0.0) || approx_eq(p.x, 100.0))
                    && (approx_eq(p.y, 0.0) || approx_eq(p.y, 100.0)),
                "vertex ({}, {}) should be a corner of the clip rect",
                p.x,
                p.y,
            );
        }
    }

    #[test]
    fn test_clip_triangle_base_below_viewport() {
        // Given a triangle with apex at (50,50) and base at y=120,
        // then bottom clip at y=100 clips the base
        let ring = vec![
            Point::new(50.0, 50.0),
            Point::new(80.0, 120.0),
            Point::new(20.0, 120.0),
        ];
        let clipped = clip_polygon_sutherland_hodgman(&ring, 0.0, 0.0, 100.0, 100.0);
        assert!(clipped.len() >= 3);
        for p in &clipped {
            assert!(p.x >= -1e-9);
            assert!(p.y >= -1e-9);
            assert!(p.x <= 100.0 + 1e-9);
            assert!(p.y <= 100.0 + 1e-9);
        }
        assert!(clipped
            .iter()
            .any(|p| approx_eq(p.x, 50.0) && approx_eq(p.y, 50.0)));
    }

    #[test]
    fn test_clip_preserves_winding_order() {
        let ring = vec![
            Point::new(10.0, 10.0),
            Point::new(90.0, 10.0),
            Point::new(90.0, 90.0),
            Point::new(10.0, 90.0),
        ];
        let clipped = clip_polygon_sutherland_hodgman(&ring, 0.0, 0.0, 100.0, 100.0);
        assert!(points_approx_eq(&clipped, &ring));

        let area_before = shoelace_area(&ring);
        let area_after = shoelace_area(&clipped);
        assert!(
            area_before.signum() == area_after.signum(),
            "winding order changed: before={area_before}, after={area_after}"
        );
    }

    #[test]
    fn test_clip_with_negative_bounds() {
        // Given negative clip bounds (as used with CLIP_MARGIN),
        // then a polygon fully inside is unchanged
        let ring = vec![
            Point::new(-2.0, -2.0),
            Point::new(5.0, -2.0),
            Point::new(5.0, 5.0),
            Point::new(-2.0, 5.0),
        ];
        let clipped = clip_polygon_sutherland_hodgman(&ring, -4.0, -4.0, 104.0, 104.0);
        assert!(points_approx_eq(&clipped, &ring));
    }
}
