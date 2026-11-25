#![forbid(unsafe_code)]

pub mod customer;
pub mod identity;
pub mod login;
pub mod oidc;
pub mod role;
pub mod staff;

pub use identity::Identity;
pub use login::LoginId;
pub use role::{Permission, Role, UnknownRole};
pub use staff::{BootstrapOwner, NewStaff, StaffUser};
