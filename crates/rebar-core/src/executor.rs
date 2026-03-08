use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::io;
use std::rc::Rc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::{Duration, Instant};

use compio_driver::Proactor;
use turbine_core::buffer::pool::IouringBufferPool;
use turbine_core::config::PoolConfig;
use turbine_core::gc::NoopHooks;

use crate::task::{self, JoinHandle, RawTask};
pub use crate::task::JoinError;

/// Configuration for the [`RebarExecutor`].
pub struct ExecutorConfig {
    /// Maximum number of task polls per tick before yielding to I/O.
    /// Default: 128.
    pub tick_budget: usize,

    /// How often to rotate the buffer pool epoch.
    /// Default: 100ms.
    pub epoch_interval: Duration,

    /// Optional turbine buffer pool configuration.
    /// `None` disables the buffer pool.
    pub pool_config: Option<PoolConfig>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            tick_budget: 128,
            epoch_interval: Duration::from_millis(100),
            pool_config: None,
        }
    }
}

/// Single-threaded cooperative task scheduler with a compio Proactor event loop
/// and optional turbine buffer pool.
///
/// `!Send` by design — each OS thread in the thread-per-core model owns
/// exactly one executor.
pub struct RebarExecutor {
    proactor: RefCell<Proactor>,
    pool: Option<IouringBufferPool<NoopHooks>>,
    run_queue: Rc<RefCell<VecDeque<Rc<RawTask>>>>,
    timers: RefCell<BTreeMap<Instant, Vec<Waker>>>,
    epoch_interval: Duration,
    last_epoch_rotation: Cell<Instant>,
    tick_budget: usize,
    /// Flag set by the root future's waker — when true, run_tick should not
    /// block in proactor.poll because the root future may have new progress.
    root_woken: Rc<Cell<bool>>,
}

thread_local! {
    static EXECUTOR: Cell<Option<*const RebarExecutor>> = const { Cell::new(None) };
}

impl RebarExecutor {
    /// Create a new executor with the given configuration.
    pub fn new(config: ExecutorConfig) -> io::Result<Self> {
        let proactor = Proactor::new()?;

        let pool = match config.pool_config {
            Some(pool_config) => {
                let p = IouringBufferPool::new(pool_config, NoopHooks)
                    .map_err(|e| io::Error::other(e.to_string()))?;
                Some(p)
            }
            None => None,
        };

        Ok(Self {
            proactor: RefCell::new(proactor),
            pool,
            run_queue: Rc::new(RefCell::new(VecDeque::new())),
            timers: RefCell::new(BTreeMap::new()),
            epoch_interval: config.epoch_interval,
            last_epoch_rotation: Cell::new(Instant::now()),
            tick_budget: config.tick_budget,
            root_woken: Rc::new(Cell::new(false)),
        })
    }

    /// Run the executor, blocking the current thread until `future` completes.
    ///
    /// Sets the thread-local executor pointer so that [`spawn`] works from
    /// within the future.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        // Install thread-local
        let prev = EXECUTOR.get();
        EXECUTOR.set(Some(self as *const RebarExecutor));

        let mut future = std::pin::pin!(future);

        // Create a waker that sets root_woken when the root future's
        // dependencies (oneshot channels, etc.) are ready. This prevents
        // run_tick from blocking in proactor.poll when there's progress to make.
        let root_waker = flag_waker(Rc::clone(&self.root_woken));
        let mut cx = Context::from_waker(&root_waker);

        let result = loop {
            self.root_woken.set(false);

            // Poll root future
            if let Poll::Ready(val) = future.as_mut().poll(&mut cx) {
                break val;
            }

            self.run_tick();
        };

        // Restore previous thread-local
        EXECUTOR.set(prev);
        result
    }

    /// Access the compio Proactor.
    pub fn proactor(&self) -> &RefCell<Proactor> {
        &self.proactor
    }

    /// Access the optional turbine buffer pool.
    pub fn pool(&self) -> Option<&IouringBufferPool<NoopHooks>> {
        self.pool.as_ref()
    }

    /// Run one tick of the event loop.
    fn run_tick(&self) {
        // Step 1: Poll ready tasks (up to tick_budget)
        self.poll_tasks();

        // Step 2: (Hook for cross-thread bridge — wired later by migration task)

        // Step 3: I/O ops are submitted by tasks calling proactor.push() directly

        // Step 4: Poll proactor for completions
        let timeout = if !self.run_queue.borrow().is_empty() || self.root_woken.get() {
            // Tasks are ready or root future was woken — don't block
            Some(Duration::ZERO)
        } else {
            let timer_timeout = self.next_timer_deadline()
                .map(|deadline| deadline.saturating_duration_since(Instant::now()));
            timer_timeout.or(Some(Duration::from_millis(1)))
        };
        let _ = self.proactor.borrow_mut().poll(timeout);

        // Step 5: Fire expired timers
        self.fire_timers();

        // Step 6: Check epoch timer for buffer pool rotation
        if let Some(pool) = &self.pool {
            let now = Instant::now();
            if now.duration_since(self.last_epoch_rotation.get()) >= self.epoch_interval {
                let _: Result<(), _> = pool.rotate();
                let _ = pool.collect();
                self.last_epoch_rotation.set(now);
            }
        }
    }

    /// Poll up to `tick_budget` tasks from the run queue.
    fn poll_tasks(&self) {
        for _ in 0..self.tick_budget {
            let task = self.run_queue.borrow_mut().pop_front();
            let Some(task) = task else { break };

            task.set_dequeued();
            let waker = task::task_waker(Rc::clone(&task), Rc::clone(&self.run_queue));
            // If the task is not done, the waker will re-enqueue it when woken
            let _completed = task.poll(&waker);
        }
    }

    /// Get the next timer deadline, if any.
    fn next_timer_deadline(&self) -> Option<Instant> {
        self.timers.borrow().keys().next().copied()
    }

    /// Fire all timers whose deadline has passed.
    fn fire_timers(&self) {
        let now = Instant::now();
        let mut timers = self.timers.borrow_mut();

        // Collect all expired entries
        let expired: Vec<Instant> = timers
            .range(..=now)
            .map(|(k, _)| *k)
            .collect();

        for deadline in expired {
            if let Some(wakers) = timers.remove(&deadline) {
                for waker in wakers {
                    waker.wake();
                }
            }
        }
    }

    /// Register a timer that will wake the given waker at the specified deadline.
    pub(crate) fn register_timer(&self, deadline: Instant, waker: Waker) {
        self.timers
            .borrow_mut()
            .entry(deadline)
            .or_default()
            .push(waker);
    }
}

/// Spawn a future onto the current executor's run queue.
///
/// # Panics
///
/// Panics if called outside of [`RebarExecutor::block_on`].
pub fn spawn<F, T>(future: F) -> JoinHandle<T>
where
    F: Future<Output = T> + 'static,
    T: 'static,
{
    with_executor(|ex| {
        let (raw, handle) = task::create_task(future);
        ex.run_queue.borrow_mut().push_back(raw);
        handle
    })
}

/// Access the current thread's executor.
///
/// # Panics
///
/// Panics if called outside of [`RebarExecutor::block_on`].
pub fn with_executor<R>(f: impl FnOnce(&RebarExecutor) -> R) -> R {
    let ptr = EXECUTOR.get().expect("no RebarExecutor on this thread — are you inside block_on()?");
    // SAFETY: The pointer is valid for the lifetime of block_on(), which is
    // guaranteed to be on the call stack above us.
    let executor = unsafe { &*ptr };
    f(executor)
}

/// Try to access the current thread's executor, returning `None` if not inside
/// [`RebarExecutor::block_on`].
pub fn try_with_executor<R>(f: impl FnOnce(&RebarExecutor) -> R) -> Option<R> {
    EXECUTOR.get().map(|ptr| {
        let executor = unsafe { &*ptr };
        f(executor)
    })
}

/// Access the thread's buffer pool via callback, if configured.
///
/// Returns `None` if no executor is active or no pool is configured.
/// The closure borrows the pool for a bounded scope within `block_on()`.
pub fn with_buffer_pool<R>(f: impl FnOnce(&IouringBufferPool<NoopHooks>) -> R) -> Option<R> {
    try_with_executor(|ex| ex.pool().map(f)).flatten()
}

/// Create a waker that sets the given flag to `true` when woken.
/// Used for the root future in `block_on` so the event loop knows
/// the root future has new progress and shouldn't block.
fn flag_waker(flag: Rc<Cell<bool>>) -> Waker {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        // clone
        |p| {
            let rc = unsafe { Rc::from_raw(p as *const Cell<bool>) };
            let cloned = Rc::clone(&rc);
            std::mem::forget(rc);
            RawWaker::new(Rc::into_raw(cloned) as *const (), &VTABLE)
        },
        // wake (consumes)
        |p| {
            let rc = unsafe { Rc::from_raw(p as *const Cell<bool>) };
            rc.set(true);
            // drop rc
        },
        // wake_by_ref
        |p| {
            let rc = unsafe { Rc::from_raw(p as *const Cell<bool>) };
            rc.set(true);
            std::mem::forget(rc);
        },
        // drop
        |p| {
            drop(unsafe { Rc::from_raw(p as *const Cell<bool>) });
        },
    );
    let raw = RawWaker::new(Rc::into_raw(flag) as *const (), &VTABLE);
    // SAFETY: The vtable correctly manages the Rc reference count.
    unsafe { Waker::from_raw(raw) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;

    fn test_executor() -> RebarExecutor {
        RebarExecutor::new(ExecutorConfig::default()).unwrap()
    }

    #[test]
    fn block_on_immediate_value() {
        let ex = test_executor();
        let val = ex.block_on(async { 42 });
        assert_eq!(val, 42);
    }

    #[test]
    fn spawn_and_await_join_handle() {
        let ex = test_executor();
        let result = ex.block_on(async {
            let handle = spawn(async { 10 + 32 });
            handle.await.unwrap()
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn spawn_multiple_tasks() {
        let ex = test_executor();
        let result = ex.block_on(async {
            let a = spawn(async { 1 });
            let b = spawn(async { 2 });
            let c = spawn(async { 3 });
            a.await.unwrap() + b.await.unwrap() + c.await.unwrap()
        });
        assert_eq!(result, 6);
    }

    #[test]
    fn spawn_chain() {
        let ex = test_executor();
        let result = ex.block_on(async {
            let h = spawn(async {
                let inner = spawn(async { 7 });
                inner.await.unwrap() * 6
            });
            h.await.unwrap()
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn tick_budget_enforcement() {
        // With a budget of 2, spawning 4 tasks should require multiple ticks
        let ex = RebarExecutor::new(ExecutorConfig {
            tick_budget: 2,
            ..Default::default()
        })
        .unwrap();

        let counter = Rc::new(Cell::new(0u32));
        let result = ex.block_on(async {
            let counter = Rc::clone(&counter);
            let mut handles = Vec::new();
            for _ in 0..4 {
                let c = Rc::clone(&counter);
                handles.push(spawn(async move {
                    c.set(c.get() + 1);
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
        });
        let _ = result;
        assert_eq!(counter.get(), 4);
    }

    #[test]
    fn timer_basic() {
        let ex = test_executor();
        let start = Instant::now();
        ex.block_on(async {
            let duration = Duration::from_millis(50);
            sleep(duration).await;
        });
        let elapsed = start.elapsed();
        // Should have waited at least ~50ms (allow some slack)
        assert!(elapsed >= Duration::from_millis(40), "elapsed: {elapsed:?}");
    }

    #[test]
    fn timer_multiple() {
        let ex = test_executor();
        let result = ex.block_on(async {
            let order = Rc::new(RefCell::new(Vec::new()));

            let o1 = Rc::clone(&order);
            let h1 = spawn(async move {
                sleep(Duration::from_millis(30)).await;
                o1.borrow_mut().push(1);
            });

            let o2 = Rc::clone(&order);
            let h2 = spawn(async move {
                sleep(Duration::from_millis(10)).await;
                o2.borrow_mut().push(2);
            });

            h1.await.unwrap();
            h2.await.unwrap();

            let v = order.borrow().clone();
            v
        });
        // Task 2 (10ms) should complete before task 1 (30ms)
        assert_eq!(result, vec![2, 1]);
    }

    #[test]
    fn with_executor_accessible_inside_block_on() {
        let ex = test_executor();
        ex.block_on(async {
            let budget = with_executor(|e| e.tick_budget);
            assert_eq!(budget, 128);
        });
    }

    #[test]
    #[should_panic(expected = "no RebarExecutor")]
    fn with_executor_panics_outside_block_on() {
        with_executor(|_| {});
    }

    #[test]
    fn try_with_executor_returns_none_outside() {
        let result = try_with_executor(|_| 42);
        assert!(result.is_none());
    }

    #[test]
    fn with_buffer_pool_returns_none_when_no_pool() {
        let ex = test_executor(); // default config has pool_config: None
        ex.block_on(async {
            let result = with_buffer_pool(|_pool| 42);
            assert!(result.is_none());
        });
    }

    #[test]
    fn with_buffer_pool_returns_some_when_configured() {
        let ex = RebarExecutor::new(ExecutorConfig {
            pool_config: Some(turbine_core::config::PoolConfig::default()),
            ..Default::default()
        })
        .unwrap();
        ex.block_on(async {
            let result = with_buffer_pool(|pool| pool.epoch());
            assert!(result.is_some());
        });
    }

    #[test]
    fn with_buffer_pool_returns_none_outside_block_on() {
        let result = with_buffer_pool(|_| 42);
        assert!(result.is_none());
    }

    /// A simple sleep future for tests that uses the executor's timer queue.
    struct Sleep {
        deadline: Instant,
        registered: Cell<bool>,
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

    fn sleep(duration: Duration) -> impl Future<Output = ()> {
        Sleep {
            deadline: Instant::now() + duration,
            registered: Cell::new(false),
        }
    }

    #[test]
    fn task_panic_does_not_crash_executor() {
        let ex = test_executor();
        let result = ex.block_on(async {
            // Spawn a task that panics
            let panicking = spawn(async { panic!("boom") });
            let _ = panicking.await; // should return Err, not crash

            // Spawn a normal task to prove the executor is still alive
            let normal = spawn(async { 42 });
            normal.await.unwrap()
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn join_handle_returns_error_on_panic() {
        let ex = test_executor();
        ex.block_on(async {
            let handle = spawn(async { panic!("boom") });
            match handle.await {
                Err(crate::task::JoinError::Panicked(msg)) => assert_eq!(msg, "boom"),
                other => panic!("expected Err(Panicked), got {:?}", other),
            }
        });
    }
}
