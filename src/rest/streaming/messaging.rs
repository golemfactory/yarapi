pub mod capture;
mod exeunit;
mod requestor;

use serde::de::DeserializeOwned;
use serde::Serialize;

pub use exeunit::{encode_message, send_to_guest, MessagingExeUnit};
pub use requestor::MessagingRequestor;

pub trait ExeUnitMessage: Serialize + DeserializeOwned + Send + Sync {}
