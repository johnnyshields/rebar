use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::channel::oneshot;

use super::spec::ShutdownStrategy;

/// Shared sliding-window restart limit check.
/// Returns `true` if within the restart limit, `false` if exceeded.
pub(crate) fn check_restart_limit(
    restart_times: &mut VecDeque<Instant>,
    max_restarts: u32,
    max_seconds: u32,
) -> bool {
    if max_restarts == 0 {
        return false;
    }

    let now = Instant::now();
    let window = Duration::from_secs(max_seconds as u64);

    restart_times.push_back(now);

    while let Some(&front) = restart_times.front() {
        if now.duration_since(front) > window {
            restart_times.pop_front();
        } else {
            break;
        }
    }

    (restart_times.len() as u32) <= max_restarts
}

/// Shared child task shutdown logic.
/// Handles BrutalKill and Timeout strategies.
pub(crate) async fn shutdown_child_task(
    strategy: &ShutdownStrategy,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<crate::task::JoinHandle<()>>,
) {
    match (strategy, shutdown_tx, join_handle) {
        (ShutdownStrategy::BrutalKill, tx, Some(handle)) => {
            // Send shutdown signal to wake the task, then drop the handle
            // to cancel it. This ensures the task's select() exits promptly
            // rather than waiting for a long-running timer to fire.
            if let Some(tx) = tx {
                let _ = tx.send(());
            }
            drop(handle);
        }
        (ShutdownStrategy::Timeout(duration), Some(tx), Some(handle)) => {
            let _ = tx.send(());
            if crate::time::timeout(*duration, handle).await.is_err() {
                // Timed out waiting for graceful shutdown
            }
        }
        _ => {}
    }
}
