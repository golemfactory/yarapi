use futures::prelude::*;
use std::path::Path;
use tokio::sync::mpsc;

use super::capture_messages::CaptureMessages;
use super::forward_to_file::ForwardToFile;
use super::forward_to_std::ForwardStd;
use super::messaging::ExeUnitMessage;

use ya_client::model::activity::RuntimeEvent;

pub trait ResultStream: Stream {
    /// Forwards stdout and stderr events to our stdout and stderr.
    /// Function doesn't consume events.
    fn forward_to_std(self) -> ForwardStd<Self>
    where
        Self: Sized,
    {
        ForwardStd::new(self)
    }

    /// Function forwards stdout and stderr from ExeUnit to specified files.
    fn forward_to_file(self, stdout: &Path, stderr: &Path) -> anyhow::Result<ForwardToFile<Self>>
    where
        Self: Sized,
    {
        ForwardToFile::new(self, stdout, stderr)
    }

    fn capture_messages<MessageType: ExeUnitMessage>(
        self,
        notifier: mpsc::UnboundedSender<MessageType>,
    ) -> CaptureMessages<Self, MessageType>
    where
        Self: Sized,
    {
        CaptureMessages::new(self, notifier)
    }
}

impl<T: Stream<Item = RuntimeEvent> + ?Sized> ResultStream for T {}
