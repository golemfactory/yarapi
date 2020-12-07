use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{self, Write};

pub trait ExeUnitMessage: Serialize + DeserializeOwned + Send + Sync {}

pub fn encode_message(msg: &impl ExeUnitMessage) -> anyhow::Result<Vec<u8>> {
    let mut data = serde_json::to_vec(msg)?;

    // Add control characters
    data.insert(0, 0x02 as u8);
    data.push(0x03 as u8);

    Ok(data)
}

pub fn send_to_guest(msg: &impl ExeUnitMessage) -> anyhow::Result<()> {
    let data = encode_message(msg)?;

    // Write atomically to stdout.
    let mut stdout = io::stdout();
    //let mut stdout = stdout.lock();
    stdout.write(data.as_ref())?;
    Ok(())
}
