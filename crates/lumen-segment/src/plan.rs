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
/// An empty result for a non-positive, non-finite, or unreasonably large duration or target: there
/// is nothing sane to segment, not an error. `total_duration_secs` comes from a probed source file
/// -- corrupted or crafted the same way the container-probing crates' own property tests exist to
/// withstand (docs/09 roadmap: "a panic on any 12-15 byte MP4 header"), it is exactly the kind of
/// value that could otherwise be `f64::INFINITY` or some other garbage magnitude. Without the
/// [`MAX_SEGMENTS`] guard below, that flows straight into `count as u64` and then
/// `Vec::with_capacity(count as usize)`, which panics outright ("capacity overflow") long before it
/// would ever try to actually allocate that much memory.
pub fn segment_durations(total_duration_secs: f64, target_secs: f64) -> Vec<f64> {
    if !total_duration_secs.is_finite() || !target_secs.is_finite() {
        return Vec::new();
    }
    if total_duration_secs <= 0.0 || target_secs <= 0.0 {
        return Vec::new();
    }
    let count = (total_duration_secs / target_secs).ceil();
    if count > MAX_SEGMENTS as f64 {
        return Vec::new();
    }
    let count = count as u64;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let start = i as f64 * target_secs;
        let end = ((i + 1) as f64 * target_secs).min(total_duration_secs);
        out.push(end - start);
    }
    out
}

/// A plan past this many segments is already absurd -- even at a 1-second target that is over 11
/// days of content -- so treating it the same as "nothing to segment" rather than actually trying to
/// build (and allocate) a plan that size costs nothing a real VOD file would ever need.
const MAX_SEGMENTS: u64 = 1_000_000;

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
    fn a_non_finite_duration_or_target_is_an_empty_plan_not_a_panic() {
        // A probed duration is exactly the kind of value a corrupted or crafted source file can hand
        // back as garbage -- this must degrade the same way a bad container header does elsewhere in
        // the codebase, not bring down whatever is planning the segmentation.
        for bad in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert_eq!(segment_durations(bad, 6.0), Vec::<f64>::new(), "duration {bad}");
            assert_eq!(segment_durations(30.0, bad), Vec::<f64>::new(), "target {bad}");
        }
    }

    #[test]
    fn an_unreasonably_long_duration_is_an_empty_plan_not_an_allocation_panic() {
        // Before the MAX_SEGMENTS guard, this exact call panicked with "capacity overflow" inside
        // `Vec::with_capacity`: `(f64::INFINITY / 6.0).ceil() as u64` saturates to u64::MAX, and
        // `Vec::<f64>::with_capacity(u64::MAX as usize)` aborts before ever touching an allocator.
        // Covered again here, distinct from the non-finite case above, because a merely *very large
        // but still finite* duration -- a garbage timestamp, a unit mixup -- hits the same allocation
        // wall without ever being non-finite.
        assert_eq!(segment_durations(f64::INFINITY, 6.0), Vec::<f64>::new());
        assert_eq!(segment_durations(1e18, 6.0), Vec::<f64>::new());
        assert_eq!(segment_durations(f64::MAX, 1.0), Vec::<f64>::new());
    }

    #[test]
    fn a_duration_at_exactly_the_segment_cap_still_produces_a_real_plan() {
        // The guard must reject what is actually unreasonable without also swallowing the largest
        // duration that is still a legitimate plan.
        let segs = segment_durations(999_999.0, 1.0);
        assert_eq!(segs.len(), 999_999);
    }

    #[test]
    fn a_duration_shorter_than_the_target_is_a_single_segment() {
        assert_eq!(segment_durations(3.0, 6.0), vec![3.0]);
    }
}
