//! Opt-in BAC contracts. No runtime, workspace writer, or provider dependencies.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
pub mod identity;
pub mod wire;
pub mod workspace;
