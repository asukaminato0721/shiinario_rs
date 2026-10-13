//! Shared native and browser execution loop. The host resumes each slice.
use crate::{executor::LocalRuntime, session::SessionState};
use anyhow::{Result, bail};
use async_executor::Task;
use std::{
    cell::RefCell,
    future::poll_fn,
    rc::Rc,
    task::{Poll, Waker},
};

const EVENT_BUDGET: usize = 10_000;

struct StepReply {
    state: SessionState,
    budget_expired: bool,
}

#[derive(Default)]
enum Phase {
    #[default]
    Ready,
    Advance,
    Reply(StepReply),
    Waiting(SessionState),
    Ended,
    Failed(String),
}

#[derive(Default)]
struct Bridge {
    phase: Phase,
    waiter: Option<Waker>,
}

#[derive(Clone, Default)]
struct HostBridge(Rc<RefCell<Bridge>>);
impl HostBridge {
    async fn advance(&self) -> StepReply {
        poll_fn(|context| {
            let mut bridge = self.0.borrow_mut();
            if let Phase::Reply(reply) = std::mem::replace(&mut bridge.phase, Phase::Advance) {
                return Poll::Ready(reply);
            }
            bridge.waiter = Some(context.waker().clone());
            Poll::Pending
        })
        .await
    }

    async fn wait_for_host(&self, state: SessionState) {
        self.0.borrow_mut().phase = Phase::Waiting(state);
        poll_fn(|context| {
            let mut bridge = self.0.borrow_mut();
            if !matches!(bridge.phase, Phase::Waiting(_)) {
                return Poll::Ready(());
            }
            bridge.waiter = Some(context.waker().clone());
            Poll::Pending
        })
        .await
    }

    fn wake(&self) {
        let waker = self.0.borrow_mut().waiter.take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

#[derive(Default)]
pub(crate) struct MainLoop {
    // Cancel the main task before the executor and bridge are dropped.
    task: Option<Task<()>>,
    runtime: LocalRuntime,
    bridge: HostBridge,
}

impl MainLoop {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_wake_callback(&self, callback: impl Fn() + Send + Sync + 'static) {
        self.runtime.set_wake_callback(callback);
    }

    pub fn is_notified(&self) -> bool {
        self.runtime.is_notified()
    }

    /// Poll ready futures and complete scoped host operations.
    /// A task wake alone does not release a pending script timer.
    pub fn resume(
        &mut self,
        release_waiter: bool,
        mut advance: impl FnMut() -> Result<SessionState>,
        mut budget_expired: impl FnMut() -> bool,
    ) -> Result<SessionState> {
        match &self.bridge.0.borrow().phase {
            Phase::Failed(message) => bail!("{message}"),
            Phase::Ended => return Ok(SessionState::Ended),
            _ => {}
        }
        if self.task.is_none() {
            let received = self.bridge.clone();
            self.task = Some(self.runtime.spawn(run_main(received)));
        }
        if release_waiter && matches!(self.bridge.0.borrow().phase, Phase::Waiting(_)) {
            self.bridge.0.borrow_mut().phase = Phase::Ready;
            self.bridge.wake();
        }
        // Bound other ready futures as well as interpreter events.
        for _ in 0..EVENT_BUDGET * 2 {
            if !self.runtime.try_tick() {
                break;
            }
            if matches!(self.bridge.0.borrow().phase, Phase::Advance) {
                let state = match advance() {
                    Ok(state) => state,
                    Err(error) => {
                        self.bridge.0.borrow_mut().phase = Phase::Failed(format!("{error:#}"));
                        self.task.take();
                        return Err(error);
                    }
                };
                let reply = StepReply {
                    state,
                    budget_expired: state == SessionState::Runnable && budget_expired(),
                };
                self.bridge.0.borrow_mut().phase = Phase::Reply(reply);
                self.bridge.wake();
            } else if matches!(self.bridge.0.borrow().phase, Phase::Ended) || budget_expired() {
                break;
            }
        }
        Ok(match self.bridge.0.borrow().phase {
            Phase::Waiting(state) => state,
            Phase::Ended => SessionState::Ended,
            _ => SessionState::Runnable,
        })
    }
}

// The compiler retains the loop position and event count across host operations.
// Host, project and session borrows end before each task poll.
async fn run_main(bridge: HostBridge) {
    let mut events = 0;
    loop {
        let step = bridge.advance().await;
        events += 1;
        if step.state == SessionState::Ended {
            bridge.0.borrow_mut().phase = Phase::Ended;
            return;
        }
        if step.state != SessionState::Runnable || step.budget_expired || events >= EVENT_BUDGET {
            bridge.wait_for_host(step.state).await;
            events = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_budget_yields_and_resets_for_the_next_slice() {
        let mut main = MainLoop::new();
        let mut calls = 0;
        for slice in 1..=2 {
            let state = main
                .resume(
                    true,
                    || {
                        calls += 1;
                        Ok(SessionState::Runnable)
                    },
                    || false,
                )
                .unwrap();
            assert_eq!(state, SessionState::Runnable);
            assert_eq!(calls, slice * EVENT_BUDGET);
        }
    }

    #[test]
    fn timers_and_time_budget_suspend_before_more_host_operations() {
        let mut main = MainLoop::new();
        let waiting = SessionState::Waiting {
            epoch: u32::MAX - 5,
            duration: 100,
        };
        let mut calls = 0;
        for expected in [waiting, SessionState::Runnable, SessionState::Ended] {
            let state = main
                .resume(
                    true,
                    || {
                        calls += 1;
                        Ok(expected)
                    },
                    || true,
                )
                .unwrap();
            assert_eq!(state, expected);
        }
        assert_eq!(calls, 3);
    }

    #[test]
    fn host_failure_stops_execution_before_another_step() {
        let mut main = MainLoop::new();
        let mut calls = 0;
        let error = main
            .resume(
                true,
                || {
                    calls += 1;
                    anyhow::bail!("host failed")
                },
                || false,
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "host failed");
        let repeated = main
            .resume(
                true,
                || {
                    calls += 1;
                    anyhow::bail!("must not execute")
                },
                || false,
            )
            .unwrap_err();
        assert_eq!(repeated.to_string(), "host failed");
        assert_eq!(calls, 1);
    }

    #[test]
    fn ready_async_tasks_do_not_release_a_script_timer() {
        let mut main = MainLoop::new();
        let waiting = SessionState::Waiting {
            epoch: 50,
            duration: 100,
        };
        assert_eq!(
            main.resume(true, || Ok(waiting), || false).unwrap(),
            waiting
        );
        let ready = Rc::new(std::cell::Cell::new(false));
        let received = ready.clone();
        let waiter = Rc::new(RefCell::new(None::<Waker>));
        let saved = waiter.clone();
        let completed = Rc::new(std::cell::Cell::new(false));
        let finished = completed.clone();
        let _task = main.runtime.spawn(async move {
            poll_fn(|context| {
                if received.get() {
                    Poll::Ready(())
                } else {
                    *saved.borrow_mut() = Some(context.waker().clone());
                    Poll::Pending
                }
            })
            .await;
            finished.set(true);
        });
        assert_eq!(
            main.resume(false, || panic!("timer released"), || false)
                .unwrap(),
            waiting
        );
        assert!(!completed.get());
        ready.set(true);
        waiter.borrow().as_ref().unwrap().wake_by_ref();
        assert!(main.is_notified());
        assert_eq!(
            main.resume(false, || panic!("timer released"), || false)
                .unwrap(),
            waiting
        );
        assert!(completed.get());
        assert!(!main.is_notified());
        assert_eq!(
            main.resume(true, || Ok(SessionState::Ended), || false)
                .unwrap(),
            SessionState::Ended
        );
    }

    #[test]
    fn self_waking_tasks_obey_the_slice_budget_during_a_script_wait() {
        let mut main = MainLoop::new();
        let waiting = SessionState::Waiting {
            epoch: 50,
            duration: 100,
        };
        main.resume(true, || Ok(waiting), || false).unwrap();
        let polls = Rc::new(std::cell::Cell::new(0));
        let polled = polls.clone();
        let _task = main.runtime.spawn(poll_fn(move |context| {
            polled.set(polled.get() + 1);
            if polled.get() == 5 {
                Poll::Ready(())
            } else {
                context.waker().wake_by_ref();
                Poll::Pending
            }
        }));
        assert_eq!(
            main.resume(false, || panic!("timer released"), || polls.get() >= 3)
                .unwrap(),
            waiting
        );
        assert_eq!(polls.get(), 3);
        assert!(main.is_notified());
        assert_eq!(
            main.resume(false, || panic!("timer released"), || false)
                .unwrap(),
            waiting
        );
        assert_eq!(polls.get(), 5);
        assert!(!main.is_notified());
    }
}
