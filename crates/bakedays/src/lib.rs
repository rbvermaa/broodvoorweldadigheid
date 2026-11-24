#![forbid(unsafe_code)]

pub mod capacity;
pub mod days;
mod error;
pub mod extras;
pub mod orders;
pub mod pickup;
pub mod range;
pub mod schedule;
pub mod status;

pub use days::{BakeDay, CopiedDay, NewBakeDay, PickupSlot, PickupSlotInput, Schedule};
pub use error::Error;
pub use extras::{ExtraInput, ProductionExtra, ProductionExtraId, Purpose};
pub use orders::{NoOrders, OrderSource};
pub use pickup::{PickupLocation, PickupLocationInput, PickupWindow, PickupWindowInput};
pub use range::{Availability, BakeDayProduct, CapacityPool};
pub use status::{BakeDayStatus, PublicStatus, Transition};
