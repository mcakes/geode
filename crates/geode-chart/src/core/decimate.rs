//! Min-max decimation that preserves each pixel column's extremes in input
//! order. Non-finite values break the polyline, including within a column.

use super::Point;

/// Emit up to two extrema per finite run within each pixel column, with
/// breaks between runs. `x` must be ordered; only the shared length of `x`
/// and `y` is read. `out` is cleared without shrinking its capacity.
pub fn decimate(x: &[f32], y: &[f64], columns: usize, out: &mut Vec<Point>) {
    out.clear();
    let n = x.len().min(y.len());
    if n == 0 || columns == 0 {
        return;
    }
    let last_col = columns - 1;
    let column_of = |px: f32| -> usize {
        // Non-positive and NaN coordinates use the first column; the
        // remaining coordinates are floored and capped at the last.
        if px <= 0.0 || px.is_nan() {
            0
        } else {
            (px.floor() as usize).min(last_col)
        }
    };
    // The open column: (column index, index of min, min y, index of max, max y).
    let mut col: Option<(usize, usize, f64, usize, f64)> = None;
    let mut pending_break = false;

    let flush = |col: &mut Option<(usize, usize, f64, usize, f64)>, out: &mut Vec<Point>| {
        if let Some((_, imin, ymin, imax, ymax)) = col.take() {
            if imin == imax {
                out.push(Point::new(x[imin], ymin as f32));
            } else if imin < imax {
                out.push(Point::new(x[imin], ymin as f32));
                out.push(Point::new(x[imax], ymax as f32));
            } else {
                out.push(Point::new(x[imax], ymax as f32));
                out.push(Point::new(x[imin], ymin as f32));
            }
        }
    };

    for i in 0..n {
        let v = y[i];
        if !v.is_finite() {
            flush(&mut col, out);
            if !out.is_empty() {
                pending_break = true;
            }
            continue;
        }
        if pending_break {
            out.push(Point::BREAK);
            pending_break = false;
        }
        let c = column_of(x[i]);
        match &mut col {
            Some((cc, imin, ymin, imax, ymax)) if *cc == c => {
                if v < *ymin {
                    *imin = i;
                    *ymin = v;
                }
                if v > *ymax {
                    *imax = i;
                    *ymax = v;
                }
            }
            _ => {
                flush(&mut col, out);
                col = Some((c, i, v, i, v));
            }
        }
    }
    flush(&mut col, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn xs(n: usize, cols: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32 * cols as f32 / n as f32).collect()
    }

    #[test]
    fn a_column_keeps_its_min_and_max_in_x_order() {
        // ten points in one column: max at index 2, min at index 7
        let x = vec![0.1; 10];
        let mut y = vec![5.0; 10];
        y[2] = 9.0;
        y[7] = 1.0;
        let mut out = Vec::new();
        decimate(&x, &y, 1, &mut out);
        assert_eq!(out, vec![Point::new(0.1, 9.0), Point::new(0.1, 1.0)]);
        // one point in a column: one output
        decimate(&[3.5], &[2.0], 8, &mut out);
        assert_eq!(out, vec![Point::new(3.5, 2.0)]);
    }

    #[test]
    fn a_nan_breaks_the_polyline() {
        let x = xs(6, 6);
        let y = [1.0, 2.0, f64::NAN, f64::NAN, 3.0, 4.0];
        let mut out = Vec::new();
        decimate(&x, &y, 6, &mut out);
        let breaks: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_break())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(breaks, vec![2], "one break for the run: {out:?}");
        assert_eq!(out.len(), 5);
        assert_eq!(out[4], Point::new(x[5], 4.0));
        // a NaN inside a column splits the column too
        // (compared field-by-field, not via `assert_eq!` on the whole vec:
        // `Point` derives `PartialEq` and `Point::BREAK` is NaN-x, and
        // IEEE 754 NaN != NaN even when the two sides print identically)
        decimate(&[0.2, 0.4, 0.6], &[1.0, f64::NAN, 2.0], 1, &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], Point::new(0.2, 1.0));
        assert!(out[1].is_break());
        assert_eq!(out[2], Point::new(0.6, 2.0));
        // never leading, trailing or doubled
        decimate(&xs(4, 4), &[f64::NAN, 1.0, f64::NAN, f64::NAN], 4, &mut out);
        assert_eq!(out, vec![Point::new(1.0, 1.0)]);
        decimate(&xs(3, 3), &[f64::NAN; 3], 3, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn out_is_reused_not_grown() {
        let mut out = Vec::with_capacity(64);
        let ptr = out.as_ptr();
        decimate(&xs(20, 10), &[1.0; 20], 10, &mut out);
        decimate(&xs(20, 10), &[2.0; 20], 10, &mut out);
        assert_eq!(out.as_ptr(), ptr, "no reallocation while capacity suffices");
        assert!(out.iter().all(|p| p.y == 2.0));
    }

    #[test]
    fn points_outside_the_columns_clamp_to_the_edge_columns() {
        let mut out = Vec::new();
        decimate(&[-3.0, 0.5, 12.0], &[1.0, 2.0, 3.0], 10, &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].x, -3.0);
        assert_eq!(out[2].y, 3.0);
    }

    proptest! {
        #[test]
        fn decimation_keeps_every_columns_min_and_max(
            n in 1usize..400,
            cols in 1usize..40,
            seed in any::<u64>(),
        ) {
            let x = xs(n, cols);
            let mut s = seed | 1;
            let y: Vec<f64> = (0..n).map(|_| {
                s ^= s << 13; s ^= s >> 7; s ^= s << 17;
                if s % 11 == 0 { f64::NAN } else { (s % 1000) as f64 / 10.0 }
            }).collect();
            let mut out = Vec::new();
            decimate(&x, &y, cols, &mut out);
            for c in 0..cols {
                let ys: Vec<f64> = (0..n).filter(|&i| (x[i].floor() as usize).min(cols - 1) == c && y[i].is_finite()).map(|i| y[i]).collect();
                if ys.is_empty() { continue; }
                let lo = ys.iter().copied().fold(f64::INFINITY, f64::min);
                let hi = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let col_out: Vec<f64> = out.iter().filter(|p| !p.is_break() && (p.x.floor() as usize).min(cols - 1) == c).map(|p| p.y as f64).collect();
                prop_assert!(col_out.iter().any(|v| (*v - lo).abs() < 1e-4), "column {c} lost its min {lo}: {col_out:?}");
                prop_assert!(col_out.iter().any(|v| (*v - hi).abs() < 1e-4), "column {c} lost its max {hi}");
                // at most two points per NaN-free run within the column
                let runs = 1 + (0..n).filter(|&i| (x[i].floor() as usize).min(cols - 1) == c).collect::<Vec<_>>().windows(2).filter(|w| y[w[0]].is_finite() != y[w[1]].is_finite()).count();
                prop_assert!(col_out.len() <= 2 * runs, "column {c}: {} points over {runs} runs", col_out.len());
            }
            prop_assert!(!out.first().is_some_and(|p| p.is_break()));
            prop_assert!(!out.last().is_some_and(|p| p.is_break()));
            prop_assert!(!out.windows(2).any(|w| w[0].is_break() && w[1].is_break()));
        }
    }
}
