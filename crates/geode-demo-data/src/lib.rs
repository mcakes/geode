//! Deterministic synthetic risk data for benchmarks, tests, and `--demo`
//! mode (spec §7.4, §10.3). Seeded: same config always yields identical
//! data. Struct-of-arrays per the performance philosophy — no row objects.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub struct GeneratorConfig {
    pub rows: usize,
    pub seed: u64,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            rows: 100_000,
            seed: 42,
        }
    }
}

/// Struct-of-arrays risk snapshot: one Vec per column, index = row.
pub struct RiskBatch {
    pub position_id: Vec<u64>,
    pub book: Vec<String>,
    pub desk: Vec<String>,
    pub model_code: Vec<String>,
    pub underlying: Vec<String>,
    pub instrument: Vec<String>,
    pub npv: Vec<f64>,
    pub pnl: Vec<f64>,
    pub delta: Vec<f64>,
    pub gamma: Vec<f64>,
    pub vega: Vec<f64>,
    pub theta: Vec<f64>,
    pub rho: Vec<f64>,
}

impl RiskBatch {
    pub fn len(&self) -> usize {
        self.position_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

const UNDERLYINGS: &[&str] = &[
    "SPX", "SX5E", "NKY", "UKX", "NDX", "RTY", "DAX", "SMI", "HSI", "KOSPI2",
];
const MODEL_CODES: &[&str] = &[
    "EURP", "AMRP", "VSWP", "AUTO", "CLIQ", "BARR", "DIGI", "VANL",
];
const DESKS: &[&str] = &["IDX_EXO_EU", "IDX_EXO_US", "IDX_EXO_AS"];
const BOOK_COUNT: usize = 20;

pub fn generate(config: &GeneratorConfig) -> RiskBatch {
    let mut rng = StdRng::seed_from_u64(config.seed);
    let n = config.rows;

    let mut batch = RiskBatch {
        position_id: Vec::with_capacity(n),
        book: Vec::with_capacity(n),
        desk: Vec::with_capacity(n),
        model_code: Vec::with_capacity(n),
        underlying: Vec::with_capacity(n),
        instrument: Vec::with_capacity(n),
        npv: Vec::with_capacity(n),
        pnl: Vec::with_capacity(n),
        delta: Vec::with_capacity(n),
        gamma: Vec::with_capacity(n),
        vega: Vec::with_capacity(n),
        theta: Vec::with_capacity(n),
        rho: Vec::with_capacity(n),
    };

    for i in 0..n {
        batch.position_id.push(i as u64);
        batch
            .book
            .push(format!("BK{:03}", rng.random_range(0..BOOK_COUNT)));
        batch
            .desk
            .push(DESKS[rng.random_range(0..DESKS.len())].to_string());
        batch
            .model_code
            .push(MODEL_CODES[rng.random_range(0..MODEL_CODES.len())].to_string());
        batch
            .underlying
            .push(UNDERLYINGS[rng.random_range(0..UNDERLYINGS.len())].to_string());
        batch.instrument.push(format!("INST{i:08}"));
        batch.npv.push(rng.random_range(-5_000_000.0..5_000_000.0));
        batch.pnl.push(rng.random_range(-500_000.0..500_000.0));
        batch.delta.push(rng.random_range(-100_000.0..100_000.0));
        batch.gamma.push(rng.random_range(-5_000.0..5_000.0));
        batch.vega.push(rng.random_range(-50_000.0..50_000.0));
        batch.theta.push(rng.random_range(-10_000.0..10_000.0));
        batch.rho.push(rng.random_range(-20_000.0..20_000.0));
    }

    batch
}

pub fn write_csv<W: std::io::Write>(batch: &RiskBatch, out: &mut W) -> std::io::Result<()> {
    writeln!(
        out,
        "position_id,book,desk,model_code,underlying,instrument,npv,pnl,delta,gamma,vega,theta,rho"
    )?;
    for i in 0..batch.len() {
        writeln!(
            out,
            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
            batch.position_id[i],
            batch.book[i],
            batch.desk[i],
            batch.model_code[i],
            batch.underlying[i],
            batch.instrument[i],
            batch.npv[i],
            batch.pnl[i],
            batch.delta[i],
            batch.gamma[i],
            batch.vega[i],
            batch.theta[i],
            batch.rho[i],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_requested_row_count() {
        let batch = generate(&GeneratorConfig {
            rows: 1_000,
            seed: 42,
        });
        assert_eq!(batch.len(), 1_000);
        assert_eq!(batch.book.len(), 1_000);
        assert_eq!(batch.npv.len(), 1_000);
    }

    #[test]
    fn same_seed_yields_identical_data() {
        let cfg = GeneratorConfig { rows: 500, seed: 7 };
        let a = generate(&cfg);
        let b = generate(&cfg);
        assert_eq!(a.position_id, b.position_id);
        assert_eq!(a.book, b.book);
        assert_eq!(a.npv, b.npv);
        assert_eq!(a.delta, b.delta);
    }

    #[test]
    fn different_seeds_yield_different_data() {
        let a = generate(&GeneratorConfig { rows: 500, seed: 1 });
        let b = generate(&GeneratorConfig { rows: 500, seed: 2 });
        assert_ne!(a.npv, b.npv);
    }

    #[test]
    fn dimensions_have_realistic_bounded_cardinality() {
        use std::collections::HashSet;
        let batch = generate(&GeneratorConfig {
            rows: 10_000,
            seed: 42,
        });
        let books: HashSet<_> = batch.book.iter().collect();
        let underlyings: HashSet<_> = batch.underlying.iter().collect();
        assert!(
            books.len() > 1 && books.len() <= 20,
            "books: {}",
            books.len()
        );
        assert!(underlyings.len() > 1 && underlyings.len() <= 10);
    }

    #[test]
    fn csv_has_header_and_one_line_per_row() {
        let batch = generate(&GeneratorConfig { rows: 10, seed: 42 });
        let mut out = Vec::new();
        write_csv(&batch, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 11);
        assert_eq!(
            lines[0],
            "position_id,book,desk,model_code,underlying,instrument,npv,pnl,delta,gamma,vega,theta,rho"
        );
        assert!(lines[1].starts_with("0,"));
    }
}
