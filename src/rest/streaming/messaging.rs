pub mod capture;
mod exeunit;

use serde::de::DeserializeOwned;
use serde::Serialize;

pub use exeunit::{encode_message, send_to_guest};

pub trait ExeUnitMessage: Serialize + DeserializeOwned + Send + Sync {}
