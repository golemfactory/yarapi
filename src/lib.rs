/* TODO don't use PaymentManager from gwasm-runner */
#[allow(dead_code)]
#[allow(unused_variables)]
#[allow(unused_must_use)]
mod payment_manager;
mod package;
mod requestor;

pub use package::Package;
pub use requestor::*;
