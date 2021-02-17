use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::ExeUnitMessage;
use crate::rest::Transfers;

#[derive(Clone)]
pub struct MessagingRequestor<ActivityImpl: Transfers> {
    activity: Arc<ActivityImpl>,
    msg_counter: Arc<AtomicUsize>,
    tracked_dir: PathBuf,
}

impl<ActivityImpl: Transfers> MessagingRequestor<ActivityImpl> {
    pub fn new(
        activity: Arc<ActivityImpl>,
        tracked_dir: &Path,
    ) -> MessagingRequestor<ActivityImpl> {
        MessagingRequestor {
            activity,
            msg_counter: Arc::new(AtomicUsize::new(0)),
            tracked_dir: tracked_dir.to_path_buf(),
        }
    }

    pub async fn send(&self, msg: &impl ExeUnitMessage) -> anyhow::Result<()> {
        let filename = format!("msg-{}", self.msg_counter.fetch_add(1, Ordering::SeqCst));
        let path = self.tracked_dir.join(filename);

        Ok(self.activity.send_json(&path, &msg).await?)
    }
}
