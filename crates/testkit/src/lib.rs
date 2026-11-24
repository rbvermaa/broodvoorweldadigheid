#![forbid(unsafe_code)]

mod clock;
#[cfg(feature = "email")]
mod email;
mod payments;

pub use clock::FakeClock;
#[cfg(feature = "email")]
pub use email::FakeEmailSender;
pub use payments::FakePaymentProvider;
