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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpsc_unbounded_roundtrip() {
        let (tx, mut rx) = mpsc::unbounded::channel::<u32>();
        tx.send(42).unwrap();
        tx.send(99).unwrap();
        assert_eq!(rx.try_recv().unwrap(), 42);
        assert_eq!(rx.try_recv().unwrap(), 99);
    }

    #[test]
    fn oneshot_roundtrip() {
        let (tx, rx) = oneshot::channel::<String>();
        tx.send("hello".into()).ok();
        // oneshot::Receiver is async; poll it via a trivial executor
        let val = futures::executor::block_on(rx).unwrap();
        assert_eq!(val, "hello");
    }

    #[test]
    fn mpsc_sender_drop_closes_receiver() {
        let (tx, mut rx) = mpsc::unbounded::channel::<u8>();
        drop(tx);
        // After all senders are dropped, try_recv should fail
        assert!(rx.try_recv().is_err());
    }
}
