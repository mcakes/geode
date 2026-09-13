//! Seeded generation. Structure first (desks → books → LHUs → positions →
//! instruments → underlyings), then measures assigned *at their grain* so
//! coarse values repeat exactly, which is what the ingest grain split and
//! conflict detector are tested against.

use crate::model::RiskBatch;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub struct GeneratorConfig {
    /// Target row count. Structure is generated until this is reached.
    pub rows: usize,
    pub seed: u64,
    /// How many consecutive business dates to spread generations over.
    pub business_dates: usize,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            rows: 100_000,
            seed: 42,
            business_dates: 3,
        }
    }
}

const UNDERLYINGS: &[&str] = &[
    "SPX", "SX5E", "NKY", "UKX", "NDX", "RTY", "DAX", "SMI", "HSI", "KOSPI2",
];

/// The generator's own underlying vocabulary (Task 10, the demo bus):
/// `demo_bus::spawn` needs one key per underlying it publishes a CVI
/// document for, and this is the one place that vocabulary is declared
/// — the risk generator's `UNDERLYINGS` above, not a second list the
/// demo bus would have to keep in step with it by hand.
pub fn demo_underlyings() -> Vec<String> {
    UNDERLYINGS.iter().map(|s| s.to_string()).collect()
}

const CURRENCIES: &[&str] = &["USD", "EUR", "JPY", "GBP"];
const MODEL_CODES: &[&str] = &[
    "EURP", "AMRP", "VSWP", "AUTO", "CLIQ", "BARR", "DIGI", "VANL",
];
const COUNTERPARTIES: &[&str] = &["CPTY_A", "CPTY_B", "CPTY_C", "CPTY_D"];
const BOOK_COUNT: usize = 20;
const LHUS_PER_BOOK: usize = 4;

/// Rows a single position contributes, on average: ~2 legs, each emitting
/// the ordered pairs of its 2-or-3 underlyings (2 rows, or 6 one time in
/// ten) — so ~4.8 in practice.
///
/// This must **underestimate**. It sizes the position loop, and generation
/// stops exactly on the requested row count; overshooting the estimate
/// means running out of positions and returning short.
const AVG_ROWS_PER_POSITION: usize = 4;

pub fn generate(config: &GeneratorConfig) -> RiskBatch {
    let mut rng = StdRng::seed_from_u64(config.seed);
    let mut b = RiskBatch::default();
    let mut position_seq: u64 = 0;

    // Book and LHU cardinality is a property of the desk and stays fixed;
    // position count is what scales with the requested row count. Without
    // this the generator caps out around 3k rows per business date and the
    // §7.4 million-row benchmarks silently measure the wrong thing.
    let dates = config.business_dates.max(1);
    let slots = dates * BOOK_COUNT * LHUS_PER_BOOK;
    let positions_per_lhu = config.rows.div_ceil(slots * AVG_ROWS_PER_POSITION).max(1);

    'outer: for date_idx in 0..dates {
        let business_date = format!("2026-08-{:02}", 24 + date_idx);
        for book_idx in 0..BOOK_COUNT {
            let book = format!("BK{book_idx:03}");
            for lhu_idx in 0..LHUS_PER_BOOK {
                let lhu = format!("{book}_LHU{lhu_idx}");
                // Positions per LHU, each with 1-3 legs.
                for _ in 0..positions_per_lhu {
                    position_seq += 1;
                    let position_ref = format!("POS{position_seq:07}");
                    let counterparty =
                        COUNTERPARTIES[rng.random_range(0..COUNTERPARTIES.len())].to_string();

                    // Position-grain measures: one value, repeated on every row.
                    let daily_trading_pnl = rng.random_range(-250_000.0..250_000.0);
                    let sc = rng.random_range(0.0..80_000.0);

                    let legs = rng.random_range(1..=3);
                    for leg in 0..legs {
                        let instrument_ref =
                            format!("{position_ref}{}", (b'a' + leg as u8) as char);

                        // Instrument reference attributes and instrument-grain
                        // measures: one value each, repeated on every row.
                        let strike = (rng.random_range(50.0..150.0f64) * 100.0).round() / 100.0;
                        let expiry = format!("2027-{:02}-15", rng.random_range(1..=12));
                        let currency =
                            CURRENCIES[rng.random_range(0..CURRENCIES.len())].to_string();
                        let model_code =
                            MODEL_CODES[rng.random_range(0..MODEL_CODES.len())].to_string();
                        let npv = rng.random_range(-5_000_000.0..5_000_000.0);
                        let daily_pnl = rng.random_range(-500_000.0..500_000.0);
                        let daily_m2m_pnl = daily_pnl * 0.8;
                        let daily_fx_pnl = daily_pnl * 0.2;
                        let clean_theta = rng.random_range(-30_000.0..0.0);
                        let realized_theta = clean_theta * 0.9;

                        // 2 or 3 underlyings: mono-underlying products still
                        // carry currency risk, so 2 is the floor (spec §3.1).
                        let n_underlying = if rng.random_range(0..10) == 0 { 3 } else { 2 };
                        let mut unders: Vec<&str> = Vec::with_capacity(n_underlying);
                        while unders.len() < n_underlying {
                            let u = UNDERLYINGS[rng.random_range(0..UNDERLYINGS.len())];
                            if !unders.contains(&u) {
                                unders.push(u);
                            }
                        }

                        // Underlying-grain measures: one value per underlying,
                        // repeated across that underlying's pair rows.
                        let per_underlying: Vec<[f64; 12]> = unders
                            .iter()
                            .map(|_| {
                                [
                                    rng.random_range(-100_000.0..100_000.0), // delta01
                                    rng.random_range(-100_000.0..100_000.0), // delta02
                                    rng.random_range(-100_000.0..100_000.0), // delta05
                                    rng.random_range(-5_000.0..5_000.0),     // gamma01
                                    rng.random_range(-5_000.0..5_000.0),     // gamma02
                                    rng.random_range(-5_000.0..5_000.0),     // gamma05
                                    rng.random_range(-50_000.0..50_000.0),   // vega01
                                    rng.random_range(-5_000.0..5_000.0),     // normalized_vega01
                                    rng.random_range(-8_000.0..8_000.0),     // skew01
                                    rng.random_range(-20_000.0..20_000.0),   // rho010
                                    rng.random_range(-20_000.0..20_000.0),   // rho_rfr010
                                    rng.random_range(-20_000.0..20_000.0),   // rho_ois010
                                ]
                            })
                            .collect();

                        // Pair-grain measures: keyed by the *canonical* pair so
                        // both orderings carry the same value (spec §3.3).
                        let mut pair_values: Vec<((usize, usize), [f64; 2])> = Vec::new();
                        for i in 0..unders.len() {
                            for j in (i + 1)..unders.len() {
                                pair_values.push((
                                    (i, j),
                                    [
                                        rng.random_range(-2_000.0..2_000.0),
                                        rng.random_range(-2_000.0..2_000.0),
                                    ],
                                ));
                            }
                        }
                        let pair_value = |i: usize, j: usize| -> [f64; 2] {
                            let key = if i < j { (i, j) } else { (j, i) };
                            pair_values
                                .iter()
                                .find(|(k, _)| *k == key)
                                .map(|(_, v)| *v)
                                .unwrap()
                        };

                        for i in 0..unders.len() {
                            for j in 0..unders.len() {
                                if i == j {
                                    continue;
                                }
                                let u = per_underlying[i];
                                let p = pair_value(i, j);
                                b.business_date.push(business_date.clone());
                                b.book.push(book.clone());
                                b.lhu.push(lhu.clone());
                                b.position_ref.push(position_ref.clone());
                                b.instrument_ref.push(instrument_ref.clone());
                                b.underlying_ref.push(unders[i].to_string());
                                b.underlying2_ref.push(unders[j].to_string());
                                b.counterparty.push(counterparty.clone());
                                b.strike.push(strike);
                                b.expiry.push(expiry.clone());
                                b.currency.push(currency.clone());
                                b.model_code.push(model_code.clone());
                                b.delta01.push(u[0]);
                                b.delta02.push(u[1]);
                                b.delta05.push(u[2]);
                                b.gamma01.push(u[3]);
                                b.gamma02.push(u[4]);
                                b.gamma05.push(u[5]);
                                b.vega01.push(u[6]);
                                b.normalized_vega01.push(u[7]);
                                b.skew01.push(u[8]);
                                b.rho010.push(u[9]);
                                b.rho_rfr010.push(u[10]);
                                b.rho_ois010.push(u[11]);
                                b.cross_gamma02.push(p[0]);
                                b.cross_gamma05.push(p[1]);
                                b.npv.push(npv);
                                b.daily_pnl.push(daily_pnl);
                                b.daily_m2m_pnl.push(daily_m2m_pnl);
                                b.daily_fx_pnl.push(daily_fx_pnl);
                                b.clean_theta_business_day.push(clean_theta);
                                b.realized_theta.push(realized_theta);
                                b.daily_trading_pnl.push(daily_trading_pnl);
                                b.sc.push(sc);
                                if b.len() >= config.rows {
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    b
}
