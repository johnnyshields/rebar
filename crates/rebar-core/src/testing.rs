//! Test utilities for rebar-core and downstream crates.
//!
//! Gated behind the `testing` Cargo feature. Use in `[dev-dependencies]`:
//! ```toml
//! rebar-core = { path = "../rebar-core", features = ["testing"] }
//! ```

use crate::executor::{ExecutorConfig, RebarExecutor};

/// Create a [`RebarExecutor`] with default config for use in `#[test]` functions.
///
/// ```rust,no_run
/// # use rebar_core::testing::test_executor;
/// #[test]
/// fn my_test() {
///     let ex = test_executor();
///     ex.block_on(async {
///         // async test body
///     });
/// }
/// ```
pub fn test_executor() -> RebarExecutor {
    RebarExecutor::new(ExecutorConfig::default()).unwrap()
}
