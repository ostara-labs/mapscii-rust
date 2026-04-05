//! RTree-based label collision detection.
//!
//! Uses 2D spatial indexing (via `rstar`) to avoid overlapping labels
//! and to find labels underneath a cursor position.
//!
//! Translated from the original mapscii `LabelBuffer.js`.

use rstar::{RTree, RTreeObject, AABB};
use unicode_width::UnicodeWidthStr;

/// A label entry stored in the spatial index.
#[derive(Debug, Clone)]
pub struct LabelEntry {
    /// Bounding box in terminal-cell coordinates.
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
    /// Optional reference to the feature that spawned this label.
    pub feature_id: Option<u64>,
}

impl RTreeObject for LabelEntry {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners([self.min_x, self.min_y], [self.max_x, self.max_y])
    }
}

/// Spatial index for label placement / collision detection.
pub struct LabelBuffer {
    tree: RTree<LabelEntry>,
    /// Default margin (in terminal cells) around labels.
    pub margin: f64,
}

impl LabelBuffer {
    /// Create a new empty label buffer.
    pub fn new() -> Self {
        Self {
            tree: RTree::new(),
            margin: 5.0,
        }
    }

    /// Remove all labels.
    pub fn clear(&mut self) {
        self.tree = RTree::new();
    }

    /// Convert pixel coordinates to terminal-cell coordinates.
    #[inline]
    pub fn project(x: f64, y: f64) -> (f64, f64) {
        ((x / 2.0).floor(), (y / 4.0).floor())
    }

    /// Try to place a label at pixel position `(x, y)`.
    ///
    /// Returns `true` if the label was placed (no collision), `false` otherwise.
    pub fn write_if_possible(
        &mut self,
        text: &str,
        x: f64,
        y: f64,
        feature_id: Option<u64>,
        margin: Option<f64>,
    ) -> bool {
        let margin = margin.unwrap_or(self.margin);
        let (cx, cy) = Self::project(x, y);

        if self.has_space(text, cx, cy, margin) {
            let entry = Self::calculate_area(text, cx, cy, margin, feature_id);
            self.tree.insert(entry);
            true
        } else {
            false
        }
    }

    /// Find all labels that contain the terminal-cell position `(x, y)`.
    pub fn features_at(&self, x: f64, y: f64) -> Vec<&LabelEntry> {
        let envelope = AABB::from_corners([x, y], [x, y]);
        self.tree.locate_in_envelope(&envelope).collect()
    }

    /// Check if there is space for a label at `(x, y)` in cell coordinates.
    fn has_space(&self, text: &str, x: f64, y: f64, margin: f64) -> bool {
        let area = Self::calculate_area(text, x, y, margin, None);
        let envelope = area.envelope();
        // Check for any intersecting entries
        self.tree.locate_in_envelope_intersecting(&envelope).next().is_none()
    }

    /// Calculate the bounding box for a label.
    fn calculate_area(
        text: &str,
        x: f64,
        y: f64,
        margin: f64,
        feature_id: Option<u64>,
    ) -> LabelEntry {
        let text_width = UnicodeWidthStr::width(text) as f64;
        LabelEntry {
            min_x: x - margin,
            min_y: y - margin / 2.0,
            max_x: x + margin + text_width,
            max_y: y + margin / 2.0,
            feature_id,
        }
    }
}

impl Default for LabelBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_buffer() {
        let buf = LabelBuffer::new();
        assert!(buf.features_at(0.0, 0.0).is_empty());
    }

    #[test]
    fn test_place_label() {
        let mut buf = LabelBuffer::new();
        assert!(buf.write_if_possible("Hello", 10.0, 10.0, None, None));
    }

    #[test]
    fn test_collision() {
        let mut buf = LabelBuffer::new();
        assert!(buf.write_if_possible("Hello", 10.0, 10.0, None, None));
        // Same position should collide
        assert!(!buf.write_if_possible("World", 10.0, 10.0, None, None));
    }

    #[test]
    fn test_no_collision_far_apart() {
        let mut buf = LabelBuffer::new();
        assert!(buf.write_if_possible("A", 0.0, 0.0, None, None));
        assert!(buf.write_if_possible("B", 200.0, 200.0, None, None));
    }

    #[test]
    fn test_clear() {
        let mut buf = LabelBuffer::new();
        buf.write_if_possible("Hello", 10.0, 10.0, None, None);
        buf.clear();
        // After clearing, same position should be available
        assert!(buf.write_if_possible("World", 10.0, 10.0, None, None));
    }

    #[test]
    fn test_project() {
        let (cx, cy) = LabelBuffer::project(20.0, 40.0);
        assert_eq!(cx, 10.0);
        assert_eq!(cy, 10.0);
    }
}
