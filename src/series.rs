//! Time series storage with a min/max mipmap (LOD) so multi-million point
//! series can be rendered at interactive frame rates regardless of zoom.

use std::sync::Arc;

/// Bucket size multiplier between consecutive LOD levels.
const LOD_FACTOR: usize = 8;
/// Stop building coarser levels once a level would have fewer than this many points.
const MIN_LOD_POINTS: usize = 2048;

#[derive(Clone)]
pub struct TimeSeries {
    pub name: String,
    pub unit: Option<String>,
    /// Raw, time-sorted (t_utc_seconds, value) points.
    raw: Arc<Vec<[f64; 2]>>,
    /// Progressively coarser min/max envelopes, finest first.
    /// Each level stores two points (min, max, ordered by which occurs first
    /// in time) per bucket of `LOD_FACTOR^(level+1)` raw points.
    lods: Vec<Arc<Vec<[f64; 2]>>>,
}

impl TimeSeries {
    pub fn from_points(name: impl Into<String>, mut points: Vec<[f64; 2]>) -> Self {
        points.sort_by(|a, b| a[0].total_cmp(&b[0]));
        let lods = build_lods(&points);
        Self {
            name: name.into(),
            unit: None,
            raw: Arc::new(points),
            lods,
        }
    }

    pub fn with_unit(mut self, unit: Option<String>) -> Self {
        self.unit = unit;
        self
    }

    /// Number of raw samples, for the "how much data is behind this line"
    /// hint in the source browser.
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    pub fn time_bounds(&self) -> Option<(f64, f64)> {
        if self.raw.is_empty() {
            None
        } else {
            Some((self.raw[0][0], self.raw[self.raw.len() - 1][0]))
        }
    }

    /// Returns a decimated slice of points covering `[t0, t1]` (plus a little
    /// padding on each side so lines don't visibly clip at the viewport edge),
    /// choosing the coarsest LOD level that still yields at least
    /// `target_points` samples in range, offset by `offset` seconds (applied
    /// to the stored raw/UTC timestamps to get "master timeline" time).
    pub fn slice_for_range(&self, t0: f64, t1: f64, offset: f64, target_points: usize) -> Vec<[f64; 2]> {
        // Translate the requested master-timeline range into this source's
        // raw (un-offset) time domain.
        let raw_t0 = t0 - offset;
        let raw_t1 = t1 - offset;

        let level: &[[f64; 2]] = self
            .lods
            .iter()
            .rev()
            .map(|l| l.as_slice())
            .find(|l| count_in_range(l, raw_t0, raw_t1) >= target_points)
            .unwrap_or(&self.raw);

        let (lo, hi) = range_indices(level, raw_t0, raw_t1);
        level[lo..hi]
            .iter()
            .map(|p| [p[0] + offset, p[1]])
            .collect()
    }

    pub fn value_bounds_in_range(&self, t0: f64, t1: f64, offset: f64) -> Option<(f64, f64)> {
        let raw_t0 = t0 - offset;
        let raw_t1 = t1 - offset;
        let (lo, hi) = range_indices(&self.raw, raw_t0, raw_t1);
        if lo >= hi {
            return None;
        }
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        // Use the coarsest LOD that still has points in range for a fast
        // approximate min/max (exact enough for autoscaling the Y axis).
        let level: &[[f64; 2]] = self
            .lods
            .iter()
            .rev()
            .map(|l| l.as_slice())
            .find(|l| count_in_range(l, raw_t0, raw_t1) >= 32)
            .unwrap_or(&self.raw);
        let (lo, hi) = range_indices(level, raw_t0, raw_t1);
        for p in &level[lo..hi] {
            min = min.min(p[1]);
            max = max.max(p[1]);
        }
        if min.is_finite() && max.is_finite() {
            Some((min, max))
        } else {
            None
        }
    }

    /// Linear interpolation of the value at a specific master-timeline instant.
    pub fn value_at(&self, t_master: f64, offset: f64) -> Option<f64> {
        let t = t_master - offset;
        if self.raw.is_empty() {
            return None;
        }
        let idx = self.raw.partition_point(|p| p[0] < t);
        if idx == 0 {
            return Some(self.raw[0][1]);
        }
        if idx >= self.raw.len() {
            return Some(self.raw[self.raw.len() - 1][1]);
        }
        let a = self.raw[idx - 1];
        let b = self.raw[idx];
        if (b[0] - a[0]).abs() < f64::EPSILON {
            return Some(b[1]);
        }
        let frac = (t - a[0]) / (b[0] - a[0]);
        Some(a[1] + (b[1] - a[1]) * frac)
    }
}

fn count_in_range(points: &[[f64; 2]], t0: f64, t1: f64) -> usize {
    let (lo, hi) = range_indices(points, t0, t1);
    hi.saturating_sub(lo)
}

fn range_indices(points: &[[f64; 2]], t0: f64, t1: f64) -> (usize, usize) {
    if points.is_empty() {
        return (0, 0);
    }
    let lo = points.partition_point(|p| p[0] < t0).saturating_sub(1);
    let hi = (points.partition_point(|p| p[0] <= t1) + 1).min(points.len());
    (lo, hi)
}

fn build_lods(raw: &[[f64; 2]]) -> Vec<Arc<Vec<[f64; 2]>>> {
    let mut levels = Vec::new();
    let mut bucket_size = LOD_FACTOR;
    // Each level has 2 points per bucket, so compare the resulting level
    // size (not the raw bucket count) against the floor.
    while (raw.len() / bucket_size) * 2 >= MIN_LOD_POINTS {
        let mut level = Vec::with_capacity((raw.len() / bucket_size + 1) * 2);
        for chunk in raw.chunks(bucket_size) {
            let mut min_i = 0usize;
            let mut max_i = 0usize;
            for (i, p) in chunk.iter().enumerate() {
                if p[1] < chunk[min_i][1] {
                    min_i = i;
                }
                if p[1] > chunk[max_i][1] {
                    max_i = i;
                }
            }
            if min_i <= max_i {
                level.push(chunk[min_i]);
                level.push(chunk[max_i]);
            } else {
                level.push(chunk[max_i]);
                level.push(chunk[min_i]);
            }
        }
        levels.push(Arc::new(level));
        bucket_size = bucket_size.saturating_mul(LOD_FACTOR);
        if bucket_size == 0 {
            break;
        }
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_matches_raw_when_small() {
        let pts: Vec<[f64; 2]> = (0..100).map(|i| [i as f64, (i as f64).sin()]).collect();
        let ts = TimeSeries::from_points("x", pts.clone());
        let sliced = ts.slice_for_range(0.0, 99.0, 0.0, 10_000);
        assert_eq!(sliced.len(), pts.len());
    }

    #[test]
    fn decimates_large_series() {
        let pts: Vec<[f64; 2]> = (0..1_000_000).map(|i| [i as f64 * 0.01, (i as f64).sin()]).collect();
        let ts = TimeSeries::from_points("big", pts);
        let sliced = ts.slice_for_range(0.0, 10_000.0, 0.0, 2000);
        assert!(sliced.len() < 20_000, "expected decimation, got {}", sliced.len());
        assert!(!sliced.is_empty());
    }

    #[test]
    fn offset_shifts_range() {
        let pts: Vec<[f64; 2]> = (0..100).map(|i| [i as f64, i as f64]).collect();
        let ts = TimeSeries::from_points("x", pts);
        let sliced = ts.slice_for_range(10.0, 20.0, 10.0, 1000);
        // raw range queried is [0,10] shifted back by offset -10
        assert!(sliced.iter().all(|p| p[0] >= 9.0 && p[0] <= 21.0));
    }
}
