use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use notify::{EmailSender, OutgoingEmail, SendError, SendFuture};

/// Keeps every email it is given instead of sending it. Failures queued with
/// [`FakeEmailSender::fail_next`] are returned first, one per send.
#[derive(Debug, Default)]
pub struct FakeEmailSender {
    sent: Mutex<Vec<OutgoingEmail>>,
    failures: Mutex<VecDeque<SendError>>,
}

impl FakeEmailSender {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Emails accepted so far, in order.
    #[must_use]
    pub fn sent(&self) -> Vec<OutgoingEmail> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn fail_next(&self, error: SendError) {
        self.failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(error);
    }
}

impl EmailSender for FakeEmailSender {
    fn send<'a>(&'a self, email: &'a OutgoingEmail) -> SendFuture<'a> {
        let failure = self
            .failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        let result = match failure {
            Some(error) => Err(error),
            None => {
                self.sent
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(email.clone());
                Ok(())
            }
        };
        Box::pin(std::future::ready(result))
    }
}
