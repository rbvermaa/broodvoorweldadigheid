#![forbid(unsafe_code)]

mod clock;
#[cfg(feature = "email")]
mod email;

pub use clock::FakeClock;
#[cfg(feature = "email")]
pub use email::FakeEmailSender;
