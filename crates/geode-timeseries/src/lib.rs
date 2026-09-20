//! The timeseries viewer module (timeseries spec §9): a tile that plots
//! series fetched on demand, composed by arithmetic expression, managed
//! through header chips and a popup, painted by `geode-chart`.

pub mod commands;
pub mod core;
