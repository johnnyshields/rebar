//! Re-exports of `local_sync` channel primitives.
//!
//! Provides a single import path (`rebar_core::channel`) for async channels
//! used throughout the crate and downstream consumers, keeping `local_sync`
//! as the sole direct dependency.

pub mod mpsc {
    pub mod unbounded {
        pub use local_sync::mpsc::unbounded::{channel, Rx, Tx};
    }
}

pub mod oneshot {
    pub use local_sync::oneshot::{channel, Receiver, Sender};
}
