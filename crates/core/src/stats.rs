//! Small, honest statistics for comparisons and A/B tests.
//!
//! - rates: Wilson score interval (correct for small n and rates near 0 or 1)
//! - difference of two rates: two-proportion z-test (pooled)
//! - latencies: nearest-rank percentiles

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// z for a two-sided 95 % interval.
pub const Z95: f64 = 1.959_963_984_540_054;

/// A success rate with its 95 % Wilson interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rate {
    pub successes: u64,
    pub n: u64,
    pub rate: f64,
    pub low: f64,
    pub high: f64,
}

impl Rate {
    pub fn new(successes: u64, n: u64) -> Self {
        let (low, high) = wilson(successes, n, Z95);
        Self {
            successes,
            n,
            rate: if n == 0 {
                0.0
            } else {
                successes as f64 / n as f64
            },
            low,
            high,
        }
    }
}

/// Wilson score interval; `(0, 1)` without data.
pub fn wilson(successes: u64, n: u64, z: f64) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let n_f = n as f64;
    let p = successes.min(n) as f64 / n_f;
    let z2 = z * z;
    let denom = 1.0 + z2 / n_f;
    let centre = (p + z2 / (2.0 * n_f)) / denom;
    let half = z * ((p * (1.0 - p) / n_f + z2 / (4.0 * n_f * n_f)).sqrt()) / denom;
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// Standard normal cumulative distribution (Abramowitz–Stegun 7.1.26,
/// absolute error < 1.5e-7 – plenty for p-values).
pub fn normal_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs() / std::f64::consts::SQRT_2);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-(x * x) / 2.0).exp();
    if x >= 0.0 {
        0.5 * (1.0 + erf)
    } else {
        0.5 * (1.0 - erf)
    }
}

/// Two-sided p-value of the pooled two-proportion z-test; `None` when a
/// sample is empty or both rates are 0 or 1 (no variance – no statement).
pub fn two_proportion_p(k1: u64, n1: u64, k2: u64, n2: u64) -> Option<f64> {
    if n1 == 0 || n2 == 0 {
        return None;
    }
    let (n1f, n2f) = (n1 as f64, n2 as f64);
    let pooled = (k1 + k2) as f64 / (n1f + n2f);
    let se = (pooled * (1.0 - pooled) * (1.0 / n1f + 1.0 / n2f)).sqrt();
    if se == 0.0 {
        return None;
    }
    let z = (k1 as f64 / n1f - k2 as f64 / n2f) / se;
    Some(2.0 * (1.0 - normal_cdf(z.abs())))
}

/// Nearest-rank percentile (`p` in 0..=100); `None` without data.
pub fn percentile(values: &[u64], p: f64) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let rank = ((p / 100.0) * v.len() as f64).ceil().max(1.0) as usize;
    Some(v[rank.min(v.len()) - 1])
}

/// Outcome of comparing two rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Difference {
    /// Too few samples for any statement.
    InsufficientData,
    /// Significant: the second is better.
    SecondBetter,
    /// Significant: the second is worse.
    SecondWorse,
    /// Enough data, no significant difference.
    NoSignificantDifference,
}

/// Compares `b` against `a` with the z-test at `alpha`, only when both have
/// at least `min_n` samples.
pub fn compare_rates(a: &Rate, b: &Rate, min_n: u64, alpha: f64) -> (Difference, Option<f64>) {
    if a.n < min_n || b.n < min_n {
        return (Difference::InsufficientData, None);
    }
    match two_proportion_p(a.successes, a.n, b.successes, b.n) {
        Some(p) if p < alpha => (
            if b.rate > a.rate {
                Difference::SecondBetter
            } else {
                Difference::SecondWorse
            },
            Some(p),
        ),
        p => (Difference::NoSignificantDifference, p),
    }
}

/// Conservative guardrail: `b` is clearly worse than `a` when their 95 %
/// intervals do not overlap (≈ p < 0.005), and both have `min_n` samples.
pub fn clearly_worse(a: &Rate, b: &Rate, min_n: u64) -> bool {
    a.n >= min_n && b.n >= min_n && b.high < a.low
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    // covers: M4-AC-06
    #[test]
    fn wilson_matches_known_values() {
        // Reference values (e.g. Newcombe 1998, method 3).
        let (l, h) = wilson(81, 263, Z95);
        assert!(close(l, 0.2553) && close(h, 0.3662), "{l} {h}");
        let (l, h) = wilson(0, 10, Z95);
        assert!(close(l, 0.0) && close(h, 0.2775), "{l} {h}");
        let (l, h) = wilson(10, 10, Z95);
        assert!(close(l, 0.7225) && close(h, 1.0), "{l} {h}");
        assert_eq!(wilson(0, 0, Z95), (0.0, 1.0));
    }

    #[test]
    fn normal_cdf_and_z_test_match_known_values() {
        assert!(close(normal_cdf(0.0), 0.5));
        assert!(close(normal_cdf(1.96), 0.975));
        assert!(close(normal_cdf(-1.0), 0.1587));
        // 60/100 vs 45/100: z ≈ 2.12, p ≈ 0.034
        let p = two_proportion_p(60, 100, 45, 100).unwrap();
        assert!(close(p, 0.0339), "{p}");
        assert_eq!(two_proportion_p(5, 5, 7, 7), None);
        assert_eq!(two_proportion_p(1, 0, 1, 2), None);
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let v = [5, 1, 4, 2, 3];
        assert_eq!(percentile(&v, 50.0), Some(3));
        assert_eq!(percentile(&v, 95.0), Some(5));
        assert_eq!(percentile(&v, 0.0), Some(1));
        assert_eq!(percentile(&[], 50.0), None);
    }

    // covers: M4-AC-06
    #[test]
    fn no_statement_below_the_minimum_sample() {
        let a = Rate::new(9, 9);
        let b = Rate::new(0, 9);
        assert_eq!(
            compare_rates(&a, &b, 10, 0.05).0,
            Difference::InsufficientData
        );
        assert!(!clearly_worse(&a, &b, 10));
        let a = Rate::new(28, 30);
        let b = Rate::new(12, 30);
        assert_eq!(compare_rates(&a, &b, 30, 0.05).0, Difference::SecondWorse);
        assert!(clearly_worse(&a, &b, 10));
        let b = Rate::new(27, 30);
        assert_eq!(
            compare_rates(&a, &b, 30, 0.05).0,
            Difference::NoSignificantDifference
        );
        assert!(!clearly_worse(&a, &b, 10));
    }

    proptest::proptest! {
        #[test]
        fn wilson_interval_contains_the_rate(k in 0u64..500, extra in 0u64..500) {
            let n = k + extra;
            let r = Rate::new(k, n);
            proptest::prop_assert!(r.low <= r.rate + 1e-12 || n == 0);
            proptest::prop_assert!(r.high >= r.rate - 1e-12);
            proptest::prop_assert!((0.0..=1.0).contains(&r.low) && (0.0..=1.0).contains(&r.high));
        }
    }
}
