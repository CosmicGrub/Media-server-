//! Splitting a total duration into a sequence of segment durations.
//!
//! **A pre-flight estimate, not a promise.** `ffmpeg`'s own HLS muxer snaps every segment boundary to
//! the nearest keyframe rather than cutting mid-GOP -- a real source's actual segment lengths after
//! running will drift from these idealized, evenly-sized boundaries by up to one GOP. This module has
//! no keyframe timing to work from (that would mean probing the source first), so what it produces is
//! useful for "roughly how many segments will this be" and for driving [`crate::command`]'s
//! `-hls_time` target, not for predicting exact output segment lengths.

/// Splits `total_duration_secs` into segments of at most `target_secs` each, computed from clean
/// multiples of `target_secs` rather than by repeated subtraction -- floating-point error from
/// subtracting a segment's length off a running remainder would otherwise drift the last few
/// segments' boundaries measurably on a long file.
///
/// An empty result for a non-positive duration or target: there is nothing to segment, not an error.
pub fn segment_durations(total_duration_secs: f64, target_secs: f64) -> Vec<f64> {
    if total_duration_secs <= 0.0 || target_secs <= 0.0 {
        return Vec::new();
    }
    let count = (total_duration_secs / target_secs).ceil() as u64;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let start = i as f64 * target_secs;
        let end = ((i + 1) as f64 * target_secs).min(total_duration_secs);
        out.push(end - start);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_multiple_produces_equal_segments() {
        let segs = segment_durations(30.0, 6.0);
        assert_eq!(segs, vec![6.0, 6.0, 6.0, 6.0, 6.0]);
    }

    #[test]
    fn a_remainder_becomes_one_shorter_final_segment() {
        let segs = segment_durations(20.0, 6.0);
        assert_eq!(segs.len(), 4);
        assert_eq!(&segs[..3], &[6.0, 6.0, 6.0]);
        assert!((segs[3] - 2.0).abs() < 1e-9, "the remainder is 2s, got {}", segs[3]);
    }

    #[test]
    fn segments_always_sum_back_to_the_total_duration() {
        for total in [1.0, 7.3, 90.0, 3723.456, 1.0 / 3.0] {
            let segs = segment_durations(total, 6.0);
            let sum: f64 = segs.iter().sum();
            assert!((sum - total).abs() < 1e-6, "total {total}: segments summed to {sum}");
        }
    }

    #[test]
    fn nothing_to_segment_is_an_empty_plan_not_an_error() {
        assert_eq!(segment_durations(0.0, 6.0), Vec::<f64>::new());
        assert_eq!(segment_durations(-5.0, 6.0), Vec::<f64>::new());
        assert_eq!(segment_durations(30.0, 0.0), Vec::<f64>::new());
        assert_eq!(segment_durations(30.0, -1.0), Vec::<f64>::new());
    }

    #[test]
    fn a_duration_shorter_than_the_target_is_a_single_segment() {
        assert_eq!(segment_durations(3.0, 6.0), vec![3.0]);
    }
}
