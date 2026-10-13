//! Shared native and browser execution loop. The host resumes each slice.
use crate::session::SessionState;
use anyhow::{Context, Result, bail};
use shiinario_core::coroutine::{Co, Coroutine, Resume};
use std::{cell::RefCell, rc::Rc};

const EVENT_BUDGET: usize = 10_000;

pub(crate) struct StepReply {
    pub state: SessionState,
    pub budget_expired: bool,
}

enum Action {
    Advance,
    Suspend(SessionState),
}

pub(crate) struct MainLoop {
    coroutine: Coroutine<Action>,
    reply: Rc<RefCell<Option<Result<StepReply>>>>,
    failure: Option<String>,
}

impl MainLoop {
    pub fn new() -> Self {
        let reply = Rc::new(RefCell::new(None));
        let received = reply.clone();
        Self {
            coroutine: Coroutine::new(|co| run_main(co, received)),
            reply,
            failure: None,
        }
    }

    /// Complete scoped host operations until the coroutine suspends or ends.
    pub fn resume(
        &mut self,
        mut advance: impl FnMut() -> Result<StepReply>,
    ) -> Result<SessionState> {
        if let Some(message) = &self.failure {
            bail!("{message}");
        }
        loop {
            let resumed = match self.coroutine.resume() {
                Ok(resumed) => resumed,
                Err(error) => {
                    self.failure = Some(format!("{error:#}"));
                    return Err(error);
                }
            };
            match resumed {
                Resume::Yielded(Action::Advance) => {
                    *self.reply.borrow_mut() = Some(advance());
                }
                Resume::Yielded(Action::Suspend(state)) => return Ok(state),
                Resume::Complete => return Ok(SessionState::Ended),
            }
        }
    }
}

// The compiler retains the loop position and event count across host operations.
// Host, project and session borrows end before each coroutine resume.
async fn run_main(co: Co<Action>, reply: Rc<RefCell<Option<Result<StepReply>>>>) -> Result<()> {
    loop {
        let mut events = 0;
        loop {
            co.yield_(Action::Advance).await;
            let step = reply
                .borrow_mut()
                .take()
                .context("missing main loop reply")??;
            events += 1;
            match step.state {
                SessionState::Ended => return Ok(()),
                SessionState::Waiting { .. } => {
                    co.yield_(Action::Suspend(step.state)).await;
                    break;
                }
                SessionState::Runnable if step.budget_expired || events >= EVENT_BUDGET => {
                    co.yield_(Action::Suspend(SessionState::Runnable)).await;
                    break;
                }
                SessionState::Runnable => {}
            }
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
                .resume(|| {
                    calls += 1;
                    Ok(StepReply {
                        state: SessionState::Runnable,
                        budget_expired: false,
                    })
                })
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
                .resume(|| {
                    calls += 1;
                    Ok(StepReply {
                        state: expected,
                        budget_expired: true,
                    })
                })
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
            .resume(|| {
                calls += 1;
                anyhow::bail!("host failed")
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "host failed");
        let repeated = main
            .resume(|| {
                calls += 1;
                anyhow::bail!("must not execute")
            })
            .unwrap_err();
        assert_eq!(repeated.to_string(), "host failed");
        assert_eq!(calls, 1);
    }
}
