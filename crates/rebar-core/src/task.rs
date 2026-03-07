use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

/// Type-erased task that the executor can poll.
pub(crate) struct RawTask {
    /// The type-erased future.
    future: RefCell<Pin<Box<dyn Future<Output = ()> + 'static>>>,
    /// Whether this task is currently in the run queue.
    enqueued: Cell<bool>,
    /// Whether this task has already completed.
    completed: Cell<bool>,
    /// Shared cancellation flag — set by JoinHandle on drop.
    cancelled: Rc<Cell<bool>>,
}

impl RawTask {
    /// Create a new RawTask wrapping a future with a shared cancellation flag.
    pub(crate) fn new(
        future: Pin<Box<dyn Future<Output = ()> + 'static>>,
        cancelled: Rc<Cell<bool>>,
    ) -> Rc<Self> {
        Rc::new(Self {
            future: RefCell::new(future),
            enqueued: Cell::new(true), // starts enqueued
            completed: Cell::new(false),
            cancelled,
        })
    }

    /// Poll the inner future. Returns true if the future completed or was cancelled.
    pub(crate) fn poll(&self, waker: &Waker) -> bool {
        if self.completed.get() || self.cancelled.get() {
            return true;
        }
        let mut cx = Context::from_waker(waker);
        let mut future = self.future.borrow_mut();
        let done = matches!(future.as_mut().poll(&mut cx), Poll::Ready(()));
        if done {
            self.completed.set(true);
        }
        done
    }

    /// Mark as no longer in the run queue.
    pub(crate) fn set_dequeued(&self) {
        self.enqueued.set(false);
    }
}

/// Create a Waker that, when woken, pushes the given RawTask back onto the
/// provided run queue. Uses Rc-based reference counting (no unsafe raw pointer
/// tricks beyond what RawWaker requires).
pub(crate) fn task_waker(
    task: Rc<RawTask>,
    run_queue: Rc<RefCell<VecDeque<Rc<RawTask>>>>,
) -> Waker {
    // Pack both Rc's into a single heap allocation
    let data = Rc::new(WakerData {
        task,
        run_queue,
    });
    let raw = Rc::into_raw(data) as *const ();
    // SAFETY: The vtable correctly manages the Rc reference count.
    unsafe { Waker::from_raw(RawWaker::new(raw, &WAKER_VTABLE)) }
}

struct WakerData {
    task: Rc<RawTask>,
    run_queue: Rc<RefCell<VecDeque<Rc<RawTask>>>>,
}

const WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
    waker_clone,
    waker_wake,
    waker_wake_by_ref,
    waker_drop,
);

unsafe fn waker_clone(data: *const ()) -> RawWaker {
    let rc = unsafe { Rc::from_raw(data as *const WakerData) };
    let cloned = Rc::clone(&rc);
    std::mem::forget(rc); // don't decrement original
    RawWaker::new(Rc::into_raw(cloned) as *const (), &WAKER_VTABLE)
}

unsafe fn waker_wake(data: *const ()) {
    let rc = unsafe { Rc::from_raw(data as *const WakerData) };
    do_wake(&rc);
    // drop rc — consumes the reference
}

unsafe fn waker_wake_by_ref(data: *const ()) {
    let rc = unsafe { Rc::from_raw(data as *const WakerData) };
    do_wake(&rc);
    std::mem::forget(rc); // don't consume the reference
}

unsafe fn waker_drop(data: *const ()) {
    drop(unsafe { Rc::from_raw(data as *const WakerData) });
}

fn do_wake(data: &WakerData) {
    if !data.task.enqueued.get() {
        data.task.enqueued.set(true);
        data.run_queue.borrow_mut().push_back(Rc::clone(&data.task));
    }
}

/// Shared state between a spawned task and its JoinHandle.
struct TaskState<T> {
    result: Cell<Option<T>>,
    waker: Cell<Option<Waker>>,
    completed: Cell<bool>,
    /// Shared cancellation flag — when set, the executor will skip this task.
    cancelled: Rc<Cell<bool>>,
}

/// Handle returned by [`spawn`](super::executor::spawn), implements `Future`.
///
/// Awaiting a `JoinHandle` yields the task's return value once the task completes.
///
/// **Drop-cancellation**: dropping a `JoinHandle` without awaiting it or detaching it
/// cancels the underlying task. Call [`detach()`](JoinHandle::detach) to prevent this.
pub struct JoinHandle<T> {
    state: Rc<TaskState<T>>,
    detached: bool,
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        if self.state.completed.get() {
            Poll::Ready(self.state.result.take().expect("JoinHandle result already taken"))
        } else {
            self.state.waker.set(Some(cx.waker().clone()));
            Poll::Pending
        }
    }
}

impl<T> JoinHandle<T> {
    /// Detach the task so it continues running even if the handle is dropped.
    /// After calling this, dropping the handle will NOT cancel the task.
    pub fn detach(mut self) {
        self.detached = true;
        // drop self — detached flag prevents cancellation
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        // If the task hasn't completed and we haven't been detached, cancel it.
        if !self.detached && !self.state.completed.get() {
            self.state.cancelled.set(true);
        }
    }
}

/// Create a (RawTask, JoinHandle) pair for the given future.
///
/// The RawTask wraps the future so that when it completes, the result is
/// stored in the shared TaskState and the JoinHandle's waker is notified.
/// Dropping the JoinHandle cancels the underlying task.
pub(crate) fn create_task<F, T>(future: F) -> (Rc<RawTask>, JoinHandle<T>)
where
    F: Future<Output = T> + 'static,
    T: 'static,
{
    let cancelled = Rc::new(Cell::new(false));

    let state = Rc::new(TaskState {
        result: Cell::new(None),
        waker: Cell::new(None),
        completed: Cell::new(false),
        cancelled: Rc::clone(&cancelled),
    });

    let state_clone = Rc::clone(&state);
    let wrapper = async move {
        let result = future.await;
        state_clone.result.set(Some(result));
        state_clone.completed.set(true);
        if let Some(waker) = state_clone.waker.take() {
            waker.wake();
        }
    };

    let raw = RawTask::new(Box::pin(wrapper), cancelled);
    let handle = JoinHandle { state, detached: false };
    (raw, handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_handle_is_pending_before_completion() {
        let (_raw, mut handle) = create_task(async { 42 });

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut handle).poll(&mut cx).is_pending());
    }

    #[test]
    fn task_completion_resolves_join_handle() {
        let run_queue = Rc::new(RefCell::new(VecDeque::new()));
        let (raw, mut handle) = create_task(async { 42 });

        // Poll the task to completion
        let waker = task_waker(Rc::clone(&raw), Rc::clone(&run_queue));
        assert!(raw.poll(&waker)); // should complete immediately

        // Now the JoinHandle should be ready
        let noop = noop_waker();
        let mut cx = Context::from_waker(&noop);
        match Pin::new(&mut handle).poll(&mut cx) {
            Poll::Ready(val) => assert_eq!(val, 42),
            Poll::Pending => panic!("expected Ready"),
        }
    }

    #[test]
    fn waker_enqueues_task() {
        let run_queue = Rc::new(RefCell::new(VecDeque::new()));
        let (raw, _handle) = create_task(async { 1 });

        raw.set_dequeued();
        assert!(run_queue.borrow().is_empty());

        let waker = task_waker(Rc::clone(&raw), Rc::clone(&run_queue));
        waker.wake_by_ref();

        assert_eq!(run_queue.borrow().len(), 1);
    }

    #[test]
    fn double_wake_does_not_double_enqueue() {
        let run_queue = Rc::new(RefCell::new(VecDeque::new()));
        let (raw, _handle) = create_task(async { 1 });

        raw.set_dequeued();
        let waker = task_waker(Rc::clone(&raw), Rc::clone(&run_queue));
        waker.wake_by_ref();
        waker.wake_by_ref(); // second wake should be a no-op

        assert_eq!(run_queue.borrow().len(), 1);
    }

    #[test]
    fn drop_cancels_task() {
        let run_queue = Rc::new(RefCell::new(VecDeque::new()));
        let (raw, handle) = create_task(async { 42 });

        // Drop the handle — this should cancel the task
        drop(handle);

        // Polling the cancelled task should return true (done)
        let waker = task_waker(Rc::clone(&raw), Rc::clone(&run_queue));
        assert!(raw.poll(&waker));
    }

    /// A minimal no-op waker for tests.
    fn noop_waker() -> Waker {
        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |p| RawWaker::new(p, &VTABLE),
            |_| {},
            |_| {},
            |_| {},
        );
        // SAFETY: The no-op vtable doesn't dereference the data pointer.
        unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
    }
}
