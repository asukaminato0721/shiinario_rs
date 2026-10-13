//! Cooperative coroutines built from stable Rust futures.
//! Only Co::yield_ suspends execution. The caller controls every resume.
use anyhow::{Context, Result};
use std::{
    cell::RefCell,
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    task::{Context as TaskContext, Poll, Waker},
};

#[derive(Debug)]
pub(crate) enum Resume<Y> {
    Yielded(Y),
    Complete,
}

pub(crate) struct Co<Y> {
    yielded: Rc<RefCell<Option<Y>>>,
}
impl<Y> Co<Y> {
    pub(crate) async fn yield_(&self, value: Y) {
        let mut value = Some(value);
        poll_fn(|_| {
            if let Some(value) = value.take() {
                *self.yielded.borrow_mut() = Some(value);
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await
    }
}

pub(crate) struct Coroutine<Y> {
    future: Option<Pin<Box<dyn Future<Output = Result<()>>>>>,
    yielded: Rc<RefCell<Option<Y>>>,
}
impl<Y: 'static> Coroutine<Y> {
    pub(crate) fn new<F: Future<Output = Result<()>> + 'static>(
        body: impl FnOnce(Co<Y>) -> F,
    ) -> Self {
        let yielded = Rc::new(RefCell::new(None));
        let future = body(Co {
            yielded: yielded.clone(),
        });
        Self {
            future: Some(Box::pin(future)),
            yielded,
        }
    }

    pub(crate) fn resume(&mut self) -> Result<Resume<Y>> {
        let future = self
            .future
            .as_mut()
            .context("coroutine already completed")?;
        match future
            .as_mut()
            .poll(&mut TaskContext::from_waker(Waker::noop()))
        {
            Poll::Pending => Ok(Resume::Yielded(
                self.yielded
                    .borrow_mut()
                    .take()
                    .context("coroutine suspended without a yield")?,
            )),
            Poll::Ready(result) => {
                self.future = None;
                result?;
                Ok(Resume::Complete)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_variables_survive_yields_and_completion_is_final() {
        let mut coroutine = Coroutine::new(|co| async move {
            let mut sum = 0;
            for value in [2, 3, 5] {
                sum += value;
                co.yield_(sum).await;
            }
            Ok(())
        });
        for expected in [2, 5, 10] {
            assert!(
                matches!(coroutine.resume().unwrap(), Resume::Yielded(value) if value == expected)
            );
        }
        assert!(matches!(coroutine.resume().unwrap(), Resume::Complete));
        assert!(coroutine.resume().is_err());
    }

    #[test]
    fn coroutine_reports_body_errors_and_releases_captured_resources() {
        let resource = Rc::new(());
        let captured = resource.clone();
        let mut coroutine = Coroutine::new(|co| async move {
            let _resource = captured;
            co.yield_(()).await;
            anyhow::bail!("body failed")
        });
        assert!(matches!(coroutine.resume().unwrap(), Resume::Yielded(())));
        assert_eq!(Rc::strong_count(&resource), 2);
        assert_eq!(coroutine.resume().unwrap_err().to_string(), "body failed");
        assert_eq!(Rc::strong_count(&resource), 1);
    }

    #[test]
    fn dropping_a_suspended_coroutine_releases_its_local_variables() {
        let resource = Rc::new(());
        let captured = resource.clone();
        let mut coroutine = Coroutine::new(|co| async move {
            co.yield_(()).await;
            drop(captured);
            Ok(())
        });
        assert!(matches!(coroutine.resume().unwrap(), Resume::Yielded(())));
        assert_eq!(Rc::strong_count(&resource), 2);
        drop(coroutine);
        assert_eq!(Rc::strong_count(&resource), 1);
    }
}
