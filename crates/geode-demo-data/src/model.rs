//! Struct-of-arrays batch at the source file's grain: one row per ordered
//! underlying pair per instrument (spec §10.3's documented reading).

/// One Vec per column, index = row. Column names are the canonical
/// (snake_case) names; `emit` maps them to the source's header spelling.
#[derive(Debug, Default)]
pub struct RiskBatch {
    // Identity
    pub business_date: Vec<String>,
    pub book: Vec<String>,
    pub lhu: Vec<String>,
    pub position_ref: Vec<String>,
    pub instrument_ref: Vec<String>,
    pub underlying_ref: Vec<String>,
    pub underlying2_ref: Vec<String>,
    pub counterparty: Vec<String>,
    // Instrument reference attributes
    pub strike: Vec<f64>,
    pub expiry: Vec<String>,
    pub currency: Vec<String>,
    pub model_code: Vec<String>,
    // Underlying-grain measures
    pub delta01: Vec<f64>,
    pub delta02: Vec<f64>,
    pub delta05: Vec<f64>,
    pub gamma01: Vec<f64>,
    pub gamma02: Vec<f64>,
    pub gamma05: Vec<f64>,
    pub vega01: Vec<f64>,
    pub normalized_vega01: Vec<f64>,
    pub skew01: Vec<f64>,
    pub rho010: Vec<f64>,
    pub rho_rfr010: Vec<f64>,
    pub rho_ois010: Vec<f64>,
    // Pair-grain measures
    pub cross_gamma02: Vec<f64>,
    pub cross_gamma05: Vec<f64>,
    // Instrument-grain measures
    pub npv: Vec<f64>,
    pub daily_pnl: Vec<f64>,
    pub daily_m2m_pnl: Vec<f64>,
    pub daily_fx_pnl: Vec<f64>,
    pub clean_theta_business_day: Vec<f64>,
    pub realized_theta: Vec<f64>,
    // Position-grain measures
    pub daily_trading_pnl: Vec<f64>,
    pub sc: Vec<f64>,
}

impl RiskBatch {
    pub fn len(&self) -> usize {
        self.position_ref.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Canonical column order. `emit` writes headers and rows in this order,
    /// and the `_USD` twin of each measure is written immediately after it.
    pub const IDENTITY: &'static [&'static str] = &[
        "business_date",
        "book",
        "lhu",
        "position_ref",
        "instrument_ref",
        "underlying_ref",
        "underlying2_ref",
        "counterparty",
        "strike",
        "expiry",
        "currency",
        "model_code",
    ];

    pub const UNDERLYING_MEASURES: &'static [&'static str] = &[
        "delta01",
        "delta02",
        "delta05",
        "gamma01",
        "gamma02",
        "gamma05",
        "vega01",
        "normalized_vega01",
        "skew01",
        "rho010",
        "rho_rfr010",
        "rho_ois010",
    ];

    pub const PAIR_MEASURES: &'static [&'static str] = &["cross_gamma02", "cross_gamma05"];

    pub const INSTRUMENT_MEASURES: &'static [&'static str] = &[
        "npv",
        "daily_pnl",
        "daily_m2m_pnl",
        "daily_fx_pnl",
        "clean_theta_business_day",
        "realized_theta",
    ];

    pub const POSITION_MEASURES: &'static [&'static str] = &["daily_trading_pnl", "sc"];

    /// Read a measure column by canonical name; used by `emit` so the
    /// writer stays a loop over names rather than 30 hand-written fields.
    pub fn measure(&self, name: &str) -> &[f64] {
        match name {
            "delta01" => &self.delta01,
            "delta02" => &self.delta02,
            "delta05" => &self.delta05,
            "gamma01" => &self.gamma01,
            "gamma02" => &self.gamma02,
            "gamma05" => &self.gamma05,
            "vega01" => &self.vega01,
            "normalized_vega01" => &self.normalized_vega01,
            "skew01" => &self.skew01,
            "rho010" => &self.rho010,
            "rho_rfr010" => &self.rho_rfr010,
            "rho_ois010" => &self.rho_ois010,
            "cross_gamma02" => &self.cross_gamma02,
            "cross_gamma05" => &self.cross_gamma05,
            "npv" => &self.npv,
            "daily_pnl" => &self.daily_pnl,
            "daily_m2m_pnl" => &self.daily_m2m_pnl,
            "daily_fx_pnl" => &self.daily_fx_pnl,
            "clean_theta_business_day" => &self.clean_theta_business_day,
            "realized_theta" => &self.realized_theta,
            "daily_trading_pnl" => &self.daily_trading_pnl,
            "sc" => &self.sc,
            other => panic!("unknown measure column '{other}'"),
        }
    }

    /// Read an identity column by canonical name. `strike` is numeric and is
    /// formatted by the caller; every other identity column is a string.
    pub fn identity(&self, name: &str) -> &[String] {
        match name {
            "business_date" => &self.business_date,
            "book" => &self.book,
            "lhu" => &self.lhu,
            "position_ref" => &self.position_ref,
            "instrument_ref" => &self.instrument_ref,
            "underlying_ref" => &self.underlying_ref,
            "underlying2_ref" => &self.underlying2_ref,
            "counterparty" => &self.counterparty,
            "expiry" => &self.expiry,
            "currency" => &self.currency,
            "model_code" => &self.model_code,
            other => panic!("'{other}' is not a string identity column"),
        }
    }
}
