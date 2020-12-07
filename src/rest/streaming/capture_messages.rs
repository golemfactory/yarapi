use core::pin::Pin;
use futures_core::ready;
use futures_core::stream::{FusedStream, Stream};
use futures_core::task::{Context, Poll};
#[cfg(feature = "sink")]
use futures_sink::Sink;
use pin_project::pin_project;
use tokio::sync::mpsc;

use ya_client::model::activity::{CommandOutput, RuntimeEvent, RuntimeEventKind};

use super::messaging::ExeUnitMessage;

struct MessageProcessor<MessageType: ExeUnitMessage> {
    notifier: mpsc::UnboundedSender<MessageType>,
    buffer: Vec<u8>,
}

#[pin_project]
#[must_use = "streams do nothing unless polled"]
pub struct CaptureMessages<St, MessageType: ExeUnitMessage> {
    #[pin]
    stream: St,
    #[pin]
    processor: MessageProcessor<MessageType>,
}

impl<St, MessageType> CaptureMessages<St, MessageType>
where
    MessageType: ExeUnitMessage,
{
    pub(crate) fn new(
        stream: St,
        notifier: mpsc::UnboundedSender<MessageType>,
    ) -> CaptureMessages<St, MessageType> {
        CaptureMessages {
            stream,
            processor: MessageProcessor {
                notifier,
                buffer: vec![],
            },
        }
    }
}

impl<MessageType: ExeUnitMessage> MessageProcessor<MessageType> {
    /// Consumes part of output related to message, passing further rest of characters.
    /// Returns None in case, when whole output was consumed.
    pub(crate) fn consume_message(&mut self, output: CommandOutput) -> Option<CommandOutput> {
        let mut output = match &output {
            CommandOutput::Str(output) => output.as_bytes(),
            CommandOutput::Bin(output) => output.as_slice(),
        };

        let mut leftovers = vec![];

        while !output.is_empty() {
            // If we have something in buffer, we are looking for end of message sign.
            // Otherwise we are looking for beginning of message.
            if self.buffer.len() > 0 {
                output = match output.iter().position(|byte| *byte == 0x03 as u8) {
                    Some(idx) => {
                        self.buffer.extend(output[..idx].iter());
                        if let Ok(msg) = self.deserialize_message(&self.buffer) {
                            // TODO: How should we handle failed send here?
                            self.notifier.send(msg).ok();
                        }
                        self.buffer.clear();
                        &output[idx + 1..]
                    }
                    // Copy all bytes. We will try to find end in next chunks of output.
                    None => {
                        self.buffer.extend(output.iter());
                        &[]
                    }
                }
            } else {
                output = match output.iter().position(|byte| *byte == 0x02 as u8) {
                    Some(idx) => {
                        leftovers.extend(output[0..idx].iter());
                        // Adding space to buffer. Next loop iteration will enter different branch
                        // and space doesn't matter when deserializing.
                        self.buffer.push(' ' as u8);
                        &output[idx + 1..]
                    }
                    // No message start. Copy all to output.
                    None => {
                        leftovers.extend(output.iter());
                        &[]
                    }
                }
            }
        }

        match leftovers.len() > 0 {
            true => Some(CommandOutput::Bin(leftovers)),
            false => None,
        }
    }

    fn deserialize_message(&self, message: &[u8]) -> anyhow::Result<MessageType> {
        Ok(serde_json::from_slice::<MessageType>(message)?)
    }
}

impl<St, MessageType> FusedStream for CaptureMessages<St, MessageType>
where
    St: FusedStream<Item = RuntimeEvent>,
    MessageType: ExeUnitMessage,
{
    fn is_terminated(&self) -> bool {
        self.stream.is_terminated()
    }
}

impl<St, MessageType> Stream for CaptureMessages<St, MessageType>
where
    St: Stream<Item = RuntimeEvent>,
    MessageType: ExeUnitMessage,
{
    type Item = RuntimeEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let res = ready!(this.stream.as_mut().poll_next(cx));
        let mut processor = this.processor.as_mut();

        let leftovers = res.map(|event| match event.kind {
            RuntimeEventKind::StdOut(output) => {
                let batch_id = event.batch_id;
                let index = event.index;
                let timestamp = event.timestamp;

                processor
                    .consume_message(output)
                    .map(|output_left| RuntimeEvent {
                        kind: RuntimeEventKind::StdOut(output_left),
                        batch_id,
                        timestamp,
                        index,
                    })
            }
            _ => Some(event),
        });

        match leftovers {
            Some(event) => Poll::Ready(event),
            None => Poll::Pending,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.stream.size_hint()
    }
}

// Forwarding impl of Sink from the underlying stream
#[cfg(feature = "sink")]
impl<St, Item, MessageType> Sink<Item> for CaptureMessages<St, MessageType>
where
    St: Stream + Sink<MessageType>,
    MessageType: ExeUnitMessage,
{
    type Error = St::Error;

    delegate_sink!(stream, Item);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::rest::streaming::messaging::encode_message;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub enum Messages {
        Progress(f64),
        Info(String),
    }

    impl ExeUnitMessage for Messages {}

    #[tokio::test]
    async fn test_messages_only_messages_single_output() {
        let msg1 = encode_message(&Messages::Progress(0.2)).unwrap();
        let msg2 = encode_message(&Messages::Progress(0.5)).unwrap();

        let content = msg1.into_iter().chain(msg2.into_iter()).collect();
        let output = CommandOutput::Bin(content);

        let (sender, mut receiver) = mpsc::unbounded_channel::<Messages>();

        let mut processor = MessageProcessor {
            notifier: sender,
            buffer: vec![],
        };

        if let Some(_) = processor.consume_message(output) {
            panic!("Should consume whole output");
        };

        let msg1 = receiver.recv().await;
        let msg2 = receiver.recv().await;

        assert!(msg1.is_some());
        assert!(msg2.is_some());

        match msg1.unwrap() {
            Messages::Progress(value) => assert_eq!(value, 0.2),
            _ => panic!("Expected Messages::Progress"),
        };

        match msg2.unwrap() {
            Messages::Progress(value) => assert_eq!(value, 0.5),
            _ => panic!("Expected Messages::Progress"),
        };
    }

    #[tokio::test]
    async fn test_messages_single_message_surrounded() {
        let msg1 = encode_message(&Messages::Progress(0.2)).unwrap();
        let before = "Pretty weather is today".to_string();
        let after = "Execution finished".to_string();

        let content = before
            .as_bytes()
            .iter()
            .chain(msg1.iter())
            .chain(after.as_bytes().iter())
            .cloned()
            .collect();
        let output = CommandOutput::Bin(content);

        let (sender, mut receiver) = mpsc::unbounded_channel::<Messages>();

        let mut processor = MessageProcessor {
            notifier: sender,
            buffer: vec![],
        };

        match processor.consume_message(output) {
            Some(output) => match output {
                CommandOutput::Bin(text) => {
                    let expected = before
                        .as_bytes()
                        .iter()
                        .chain(after.as_bytes().iter())
                        .cloned()
                        .collect::<Vec<u8>>();
                    assert_eq!(text, expected);
                }
                _ => panic!("Expected bin output."),
            },
            None => panic!("Expected not empty output."),
        };

        let msg1 = receiver.recv().await;

        assert!(msg1.is_some());
        match msg1.unwrap() {
            Messages::Progress(value) => assert_eq!(value, 0.2),
            _ => panic!("Expected Messages::Progress"),
        };
    }
}
