//! A local executor driven by native or browser host callbacks.
use async_executor::{LocalExecutor, Task};
use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Wake, Waker},
};

type Callback = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Signal {
    notified: AtomicBool,
    callback: Mutex<Option<Callback>>,
}
impl Signal {
    fn notify(&self) {
        let was_notified = self.notified.swap(true, Ordering::AcqRel);
        if !was_notified {
            self.notify_host();
        }
    }

    fn notify_host(&self) {
        // Release the lock before the callback can request another host wake.
        let callback = self.callback.lock().unwrap().clone();
        if let Some(callback) = callback {
            callback();
        }
    }
}

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notify();
    }
}

type Tick = Pin<Box<dyn Future<Output = ()>>>;

fn next_tick(executor: Rc<LocalExecutor<'static>>) -> Tick {
    Box::pin(async move { executor.tick().await })
}

/// Runs local futures without an interpreter thread or a separate timer clock.
/// The host supplies timer and input futures and handles task wake callbacks.
pub struct LocalRuntime {
    // Drop the pending ticker before releasing the executor it borrows.
    tick: RefCell<Tick>,
    executor: Rc<LocalExecutor<'static>>,
    signal: Arc<Signal>,
}
impl Default for LocalRuntime {
    fn default() -> Self {
        let executor = Rc::new(LocalExecutor::new());
        Self {
            tick: RefCell::new(next_tick(executor.clone())),
            executor,
            signal: Arc::default(),
        }
    }
}
impl LocalRuntime {
    /// Schedule a future that can retain non-Send state on the host thread.
    /// Retain the task handle to permit cancellation when its owner stops.
    pub fn spawn<F: Future + 'static>(&self, future: F) -> Task<F::Output> {
        let task = self.executor.spawn(future);
        self.signal.notify();
        task
    }

    /// Connect task readiness to the host's event loop.
    pub fn set_wake_callback(&self, callback: impl Fn() + Send + Sync + 'static) {
        *self.signal.callback.lock().unwrap() = Some(Arc::new(callback));
        if self.is_notified() {
            self.signal.notify_host();
        }
    }

    /// Poll one queued task. A false result means the queue was empty.
    pub fn try_tick(&self) -> bool {
        self.signal.notified.store(false, Ordering::Release);
        // Keep the sleeping ticker registered. Ready tasks need no new future.
        let ran = self.executor.try_tick() || {
            let waker = Waker::from(self.signal.clone());
            let mut tick = self.tick.borrow_mut();
            let ready = tick
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_ready();
            if ready {
                *tick = next_tick(self.executor.clone());
            }
            ready
        };
        if ran {
            // There may be more queued tasks. A final empty poll clears this.
            self.signal.notified.store(true, Ordering::Release);
        }
        ran
    }

    /// A notification can require an early host wake before the next timer.
    pub fn is_notified(&self) -> bool {
        self.signal.notified.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        future::poll_fn,
        rc::Rc,
        sync::atomic::AtomicUsize,
        task::Poll,
    };

    #[test]
    fn external_wakers_resume_non_send_futures_and_notify_the_host_once() {
        let runtime = LocalRuntime::default();
        let notifications = Arc::new(AtomicUsize::new(0));
        let count = notifications.clone();
        runtime.set_wake_callback(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let ready = Rc::new(Cell::new(false));
        let received = ready.clone();
        let waiter = Rc::new(RefCell::new(None::<Waker>));
        let saved = waiter.clone();
        let polls = Rc::new(Cell::new(0));
        let polled = polls.clone();
        let completed = Rc::new(Cell::new(false));
        let finished = completed.clone();
        let _task = runtime.spawn(async move {
            let local = Rc::new(42);
            poll_fn(|context| {
                polled.set(polled.get() + 1);
                if received.get() {
                    Poll::Ready(())
                } else {
                    *saved.borrow_mut() = Some(context.waker().clone());
                    Poll::Pending
                }
            })
            .await;
            assert_eq!(*local, 42);
            finished.set(true);
        });
        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        assert!(runtime.try_tick());
        assert!(!runtime.try_tick());
        assert_eq!(polls.get(), 1);
        assert!(!completed.get());

        let wake = waiter.borrow().as_ref().unwrap().clone();
        wake.wake_by_ref();
        wake.wake_by_ref();
        assert_eq!(notifications.load(Ordering::SeqCst), 2);
        assert!(runtime.try_tick());
        assert!(!runtime.try_tick());
        assert_eq!(polls.get(), 2);

        ready.set(true);
        std::thread::spawn(move || wake.wake()).join().unwrap();
        assert_eq!(notifications.load(Ordering::SeqCst), 3);
        assert!(runtime.try_tick());
        assert!(!runtime.try_tick());
        assert!(completed.get());
        assert_eq!(polls.get(), 3);
    }

    #[test]
    fn cancelling_a_pending_task_releases_its_captured_resources() {
        let runtime = LocalRuntime::default();
        let notifications = Arc::new(AtomicUsize::new(0));
        let count = notifications.clone();
        runtime.set_wake_callback(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let resource = Rc::new(());
        let captured = resource.clone();
        let waiter = Rc::new(RefCell::new(None::<Waker>));
        let saved = waiter.clone();
        let task = runtime.spawn(async move {
            let _resource = captured;
            poll_fn::<(), _>(|context| {
                *saved.borrow_mut() = Some(context.waker().clone());
                Poll::Pending
            })
            .await;
        });
        assert!(runtime.try_tick());
        assert!(!runtime.try_tick());
        assert_eq!(Rc::strong_count(&resource), 2);
        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        drop(task);
        assert!(runtime.is_notified());
        assert_eq!(notifications.load(Ordering::SeqCst), 2);
        while runtime.try_tick() {}
        assert_eq!(Rc::strong_count(&resource), 1);
        waiter.borrow().as_ref().unwrap().wake_by_ref();
        assert!(!runtime.try_tick());
    }

    #[test]
    fn dropping_the_runtime_releases_detached_pending_tasks() {
        let runtime = LocalRuntime::default();
        let resource = Rc::new(());
        let captured = resource.clone();
        let waiter = Rc::new(RefCell::new(None::<Waker>));
        let saved = waiter.clone();
        runtime
            .spawn(async move {
                let _resource = captured;
                poll_fn::<(), _>(|context| {
                    *saved.borrow_mut() = Some(context.waker().clone());
                    Poll::Pending
                })
                .await;
            })
            .detach();
        assert!(runtime.try_tick());
        assert_eq!(Rc::strong_count(&resource), 2);
        drop(runtime);
        assert_eq!(Rc::strong_count(&resource), 1);
        waiter.borrow().as_ref().unwrap().wake_by_ref();
    }

    #[test]
    fn ticks_preserve_queued_work_without_repeated_host_notifications() {
        let runtime = LocalRuntime::default();
        let notifications = Arc::new(AtomicUsize::new(0));
        let count = notifications.clone();
        runtime.set_wake_callback(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let completed = Rc::new(Cell::new(0));
        let first = completed.clone();
        let _first = runtime.spawn(async move {
            first.set(first.get() + 1);
        });
        let second = completed.clone();
        let _second = runtime.spawn(async move {
            second.set(second.get() + 1);
        });
        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        assert!(runtime.try_tick());
        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        assert!(runtime.is_notified());
        assert!(runtime.try_tick());
        assert!(!runtime.try_tick());
        assert_eq!(completed.get(), 2);
        assert!(!runtime.is_notified());
    }
}
