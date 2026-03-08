use std::cell::Cell;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crate::executor::with_executor;

// ---------------------------------------------------------------------------
// Sleep
// ---------------------------------------------------------------------------

/// A future that completes after a specified duration.
///
/// Created by [`sleep`]. Uses the executor's timer queue — no background
/// thread or OS timer. `!Send` by design (thread-per-core model).
pub struct Sleep {
    deadline: Instant,
    registered: Cell<bool>,
}

impl Sleep {
    /// Create a new sleep future that completes at the given `deadline`.
    pub fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            registered: Cell::new(false),
        }
    }

    /// Return the instant at which this sleep will complete.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if Instant::now() >= self.deadline {
            return Poll::Ready(());
        }

        if !self.registered.get() {
            with_executor(|ex| {
                ex.register_timer(self.deadline, cx.waker().clone());
            });
            self.registered.set(true);
        }

        Poll::Pending
    }
}

/// Returns a future that completes after the specified `duration`.
///
/// # Panics
///
/// Panics if called outside of [`RebarExecutor::block_on`].
///
/// # Examples
///
/// ```ignore
/// use std::time::Duration;
/// use rebar_core::time::sleep;
///
/// sleep(Duration::from_millis(100)).await;
/// ```
pub fn sleep(duration: Duration) -> Sleep {
    Sleep::new(Instant::now() + duration)
}

/// Returns a future that completes at the specified `deadline`.
///
/// # Panics
///
/// Panics if called outside of [`RebarExecutor::block_on`].
pub fn sleep_until(deadline: Instant) -> Sleep {
    Sleep::new(deadline)
}

// ---------------------------------------------------------------------------
// Elapsed (timeout error)
// ---------------------------------------------------------------------------

/// Error returned when a [`timeout`] expires before the inner future completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "deadline has elapsed")
    }
}

impl std::error::Error for Elapsed {}

// ---------------------------------------------------------------------------
// Timeout
// ---------------------------------------------------------------------------

/// A future that completes with the inner future's output, or returns
/// [`Elapsed`] if the deadline passes first.
///
/// Created by [`timeout`].
pub struct Timeout<F> {
    future: F,
    deadline: Instant,
    timer_registered: Cell<bool>,
}

impl<F: Future> Timeout<F> {
    /// Create a new timeout wrapping `future` with the given `deadline`.
    pub fn new(future: F, deadline: Instant) -> Self {
        Self {
            future,
            deadline,
            timer_registered: Cell::new(false),
        }
    }
}

impl<F: Future> Future for Timeout<F> {
    type Output = Result<F::Output, Elapsed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: We only project to `future` which is structurally pinned.
        // `deadline` and `timer_registered` are not moved.
        let this = unsafe { self.get_unchecked_mut() };

        // Poll the inner future first — if it's ready, return immediately
        // regardless of deadline.
        let future = unsafe { Pin::new_unchecked(&mut this.future) };
        if let Poll::Ready(val) = future.poll(cx) {
            return Poll::Ready(Ok(val));
        }

        // Check if we've timed out
        if Instant::now() >= this.deadline {
            return Poll::Ready(Err(Elapsed));
        }

        // Register the timer if we haven't yet
        if !this.timer_registered.get() {
            with_executor(|ex| {
                ex.register_timer(this.deadline, cx.waker().clone());
            });
            this.timer_registered.set(true);
        }

        Poll::Pending
    }
}

/// Wraps a future with a timeout, returning [`Elapsed`] if the `duration`
/// elapses before the future completes.
///
/// # Panics
///
/// Panics if called outside of [`RebarExecutor::block_on`].
///
/// # Examples
///
/// ```ignore
/// use std::time::Duration;
/// use rebar_core::time::{timeout, sleep};
///
/// // This will return Err(Elapsed)
/// let result = timeout(Duration::from_millis(10), sleep(Duration::from_secs(10))).await;
/// assert!(result.is_err());
/// ```
pub fn timeout<F: Future>(duration: Duration, future: F) -> Timeout<F> {
    Timeout::new(future, Instant::now() + duration)
}

/// Wraps a future with a deadline, returning [`Elapsed`] if the `deadline`
/// passes before the future completes.
pub fn timeout_at<F: Future>(deadline: Instant, future: F) -> Timeout<F> {
    Timeout::new(future, deadline)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{spawn, ExecutorConfig, RebarExecutor};
    use std::rc::Rc;

    fn test_executor() -> RebarExecutor {
        RebarExecutor::new(ExecutorConfig::default()).unwrap()
    }

    #[test]
    fn sleep_zero_completes_immediately() {
        let ex = test_executor();
        ex.block_on(async {
            sleep(Duration::ZERO).await;
        });
    }

    #[test]
    fn sleep_completes_after_duration() {
        let ex = test_executor();
        let start = Instant::now();
        ex.block_on(async {
            sleep(Duration::from_millis(50)).await;
        });
        assert!(start.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn sleep_until_works() {
        let ex = test_executor();
        let start = Instant::now();
        let deadline = start + Duration::from_millis(50);
        ex.block_on(async {
            sleep_until(deadline).await;
        });
        assert!(Instant::now() >= deadline);
    }

    #[test]
    fn sleep_ordering_preserved() {
        let ex = test_executor();
        let result = ex.block_on(async {
            let order = Rc::new(std::cell::RefCell::new(Vec::new()));

            let o1 = Rc::clone(&order);
            let h1 = spawn(async move {
                sleep(Duration::from_millis(40)).await;
                o1.borrow_mut().push(1);
            });

            let o2 = Rc::clone(&order);
            let h2 = spawn(async move {
                sleep(Duration::from_millis(10)).await;
                o2.borrow_mut().push(2);
            });

            h1.await.unwrap();
            h2.await.unwrap();
            order.borrow().clone()
        });
        assert_eq!(result, vec![2, 1]);
    }

    #[test]
    fn timeout_completes_in_time() {
        let ex = test_executor();
        let result = ex.block_on(async {
            timeout(Duration::from_secs(1), async { 42 }).await
        });
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn timeout_expires() {
        let ex = test_executor();
        let result = ex.block_on(async {
            timeout(Duration::from_millis(10), sleep(Duration::from_secs(10))).await
        });
        assert_eq!(result, Err(Elapsed));
    }

    #[test]
    fn timeout_at_works() {
        let ex = test_executor();
        let deadline = Instant::now() + Duration::from_millis(10);
        let result = ex.block_on(async {
            timeout_at(deadline, sleep(Duration::from_secs(10))).await
        });
        assert_eq!(result, Err(Elapsed));
    }

    #[test]
    fn timeout_inner_completes_before_deadline() {
        let ex = test_executor();
        let result = ex.block_on(async {
            timeout(Duration::from_secs(1), async {
                sleep(Duration::from_millis(10)).await;
                "done"
            })
            .await
        });
        assert_eq!(result, Ok("done"));
    }

    #[test]
    fn elapsed_display() {
        assert_eq!(format!("{}", Elapsed), "deadline has elapsed");
    }

    #[test]
    fn elapsed_is_error() {
        let err: Box<dyn std::error::Error> = Box::new(Elapsed);
        assert_eq!(err.to_string(), "deadline has elapsed");
    }

    #[test]
    fn multiple_concurrent_sleeps() {
        let ex = test_executor();
        let start = Instant::now();
        ex.block_on(async {
            let h1 = spawn(async { sleep(Duration::from_millis(50)).await; });
            let h2 = spawn(async { sleep(Duration::from_millis(50)).await; });
            let h3 = spawn(async { sleep(Duration::from_millis(50)).await; });
            h1.await.unwrap();
            h2.await.unwrap();
            h3.await.unwrap();
        });
        // All 3 sleeps run concurrently, so total should be ~50ms, not 150ms
        assert!(start.elapsed() < Duration::from_millis(120));
    }

    #[test]
    fn timeout_with_zero_duration() {
        let ex = test_executor();
        // An immediately-ready future should still succeed even with zero timeout
        let result = ex.block_on(async {
            timeout(Duration::ZERO, async { 42 }).await
        });
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn nested_timeouts() {
        let ex = test_executor();
        let result = ex.block_on(async {
            let outer = timeout(Duration::from_secs(1), async {
                let inner = timeout(Duration::from_millis(10), sleep(Duration::from_secs(10))).await;
                inner
            })
            .await;
            outer
        });
        // Inner timeout should fire, outer should succeed
        assert_eq!(result, Ok(Err(Elapsed)));
    }

    #[test]
    fn sleep_deadline_accessor() {
        let before = Instant::now();
        let s = sleep(Duration::from_millis(100));
        let after = Instant::now();
        assert!(s.deadline() >= before + Duration::from_millis(100));
        assert!(s.deadline() <= after + Duration::from_millis(100));
    }
}
