#![forbid(unsafe_code)]

mod clock;
mod payments;

pub use clock::FakeClock;
pub use payments::FakePaymentProvider;
