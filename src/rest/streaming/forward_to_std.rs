use core::pin::Pin;
use futures_core::ready;
use futures_core::stream::{FusedStream, Stream};
use futures_core::task::{Context, Poll};
#[cfg(feature = "sink")]
use futures_sink::Sink;
use pin_project::pin_project;
use std::io::{self, Write};

use ya_client::model::activity::{CommandOutput, RuntimeEvent, RuntimeEventKind};

/// Stream for the [`forward_to_std`](super::ResultStream::forward_to_std) method.
#[pin_project]
#[must_use = "streams do nothing unless polled"]
pub struct ForwardStd<St> {
    #[pin]
    stream: St,
}

impl<St> ForwardStd<St> {
    pub(crate) fn new(stream: St) -> ForwardStd<St> {
        ForwardStd { stream }
    }
}

impl<St> FusedStream for ForwardStd<St>
where
    St: FusedStream<Item = RuntimeEvent>,
{
    fn is_terminated(&self) -> bool {
        self.stream.is_terminated()
    }
}

impl<St> Stream for ForwardStd<St>
where
    St: Stream<Item = RuntimeEvent>,
{
    type Item = RuntimeEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let res = ready!(this.stream.as_mut().poll_next(cx));

        Poll::Ready(res.map(|event| {
            match &event.kind {
                RuntimeEventKind::StdOut(output) => write_to(&mut io::stdout(), &output).ok(),
                RuntimeEventKind::StdErr(output) => write_to(&mut io::stderr(), &output).ok(),
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
impl<St, Item> Sink<Item> for ForwardStd<St>
where
    St: Stream + Sink<Item>,
{
    type Error = St::Error;

    delegate_sink!(stream, Item);
}

pub(crate) fn write_to<OutType: Write>(
    stream: &mut OutType,
    output: &CommandOutput,
) -> anyhow::Result<()> {
    match output {
        CommandOutput::Bin(output) => stream.write(output.as_ref())?,
        CommandOutput::Str(output) => stream.write(output.as_ref())?,
    };
    Ok(())
}
