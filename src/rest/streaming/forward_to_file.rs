use core::pin::Pin;
use futures_core::ready;
use futures_core::stream::{FusedStream, Stream};
use futures_core::task::{Context, Poll};
#[cfg(feature = "sink")]
use futures_sink::Sink;
use pin_project::pin_project;
use std::fs::File;
use std::path::Path;

use ya_client::model::activity::{RuntimeEvent, RuntimeEventKind};

use super::forward_to_std::write_to;

/// Stream for the [`forward_to_file`](super::ResultStream::forward_to_file) method.
#[pin_project]
#[must_use = "streams do nothing unless polled"]
pub struct ForwardToFile<St> {
    #[pin]
    stream: St,
    stdout_file: File,
    stderr_file: File,
}

impl<St> ForwardToFile<St> {
    pub(crate) fn new(
        stream: St,
        stdout: &Path,
        stderr: &Path,
    ) -> anyhow::Result<ForwardToFile<St>> {
        let stdout_file = File::create(stdout)?;
        let stderr_file = File::create(stderr)?;

        Ok(ForwardToFile {
            stream,
            stdout_file,
            stderr_file,
        })
    }
}

impl<St> FusedStream for ForwardToFile<St>
where
    St: FusedStream<Item = RuntimeEvent>,
{
    fn is_terminated(&self) -> bool {
        self.stream.is_terminated()
    }
}

impl<St> Stream for ForwardToFile<St>
where
    St: Stream<Item = RuntimeEvent>,
{
    type Item = RuntimeEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let res = ready!(this.stream.as_mut().poll_next(cx));

        Poll::Ready(res.map(|event| {
            match &event.kind {
                RuntimeEventKind::StdOut(output) => write_to(&mut this.stdout_file, &output).ok(),
                RuntimeEventKind::StdErr(output) => write_to(&mut this.stderr_file, &output).ok(),
                _ => None,
            };
            event
        }))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.stream.size_hint()
    }
}

// Forwarding impl of Sink from the underlying stream
#[cfg(feature = "sink")]
impl<St, Item> Sink<Item> for ForwardToFile<St>
where
    St: Stream + Sink<Item>,
{
    type Error = St::Error;

    delegate_sink!(stream, Item);
}
