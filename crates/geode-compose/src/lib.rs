//! The gpui-free half of Geode's composition root. The app and the
//! background collector build their store configuration from these
//! functions, so the two agree on what the store holds.

pub mod paths;

pub use paths::{config_dirs, db_path, user_config_dir};
