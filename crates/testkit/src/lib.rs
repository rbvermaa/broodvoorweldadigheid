#![forbid(unsafe_code)]

mod clock;
#[cfg(feature = "email")]
mod email;
pub mod oidc;
mod oidc_keys;
mod payments;

pub use clock::FakeClock;
#[cfg(feature = "email")]
pub use email::FakeEmailSender;
pub use oidc::{FakeOidc, FakeUser, Sabotage};
pub use payments::FakePaymentProvider;
