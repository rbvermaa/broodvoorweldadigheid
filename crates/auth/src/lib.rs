#![forbid(unsafe_code)]

pub mod oidc;
pub mod role;
pub mod staff;

pub use role::{Permission, Role, UnknownRole};
pub use staff::{BootstrapOwner, NewStaff, StaffUser};
