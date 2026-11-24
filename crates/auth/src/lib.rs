#![forbid(unsafe_code)]

pub mod identity;
pub mod role;
pub mod staff;

pub use identity::Identity;
pub use role::{Permission, Role, UnknownRole};
pub use staff::{BootstrapOwner, NewStaff, StaffUser};
