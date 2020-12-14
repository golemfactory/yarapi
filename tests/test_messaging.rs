mod testing;

use testing::prepare_test_dir;

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::Path;

use std::time::Duration;
use yarapi::rest::streaming::{ExeUnitMessage, MessagingExeUnit};

#[derive(Serialize, Deserialize)]
pub enum Messages {
    Progress(f64),
    Info(String),
}

impl ExeUnitMessage for Messages {}

fn emulate_send(msg: impl ExeUnitMessage, dir: &Path, idx: u32) -> anyhow::Result<()> {
    let path = dir.join(format!("msg-{}.json", idx));
    let file = File::create(path)?;

    Ok(serde_json::to_writer(file, &msg)?)
}

#[actix_rt::test]
async fn test_messaging_to_exeunit() -> anyhow::Result<()> {
    let dir = prepare_test_dir("test_messaging_to_exeunit")?;
    env_logger::init();

    let receiver = MessagingExeUnit::new(&dir).unwrap();
    let mut events = receiver.listen::<Messages>();

    emulate_send(Messages::Progress(0.02), &dir, 1).unwrap();

    let msg = tokio::time::timeout(Duration::from_millis(150), events.recv())
        .await?
        .unwrap();
    match msg {
        Messages::Progress(progress) => assert_eq!(progress, 0.02),
        _ => panic!("Expected Messages::Progress"),
    };

    emulate_send(Messages::Progress(0.06), &dir, 2).unwrap();

    let msg = tokio::time::timeout(Duration::from_millis(150), events.recv())
        .await?
        .unwrap();
    match msg {
        Messages::Progress(progress) => assert_eq!(progress, 0.06),
        _ => panic!("Expected Messages::Progress"),
    };

    Ok(())
}
