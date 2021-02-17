use anyhow::anyhow;
use notify::{raw_watcher, Op, RecursiveMode, Watcher};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use super::ExeUnitMessage;

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

pub struct MessagingExeUnit {
    new_message_notifier: broadcast::Sender<PathBuf>,
}

impl MessagingExeUnit {
    pub fn new(tracked_dir: &Path) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&tracked_dir).map_err(|e| {
            anyhow!(
                "Can't create directory [{}] for messages. {}",
                &tracked_dir.display(),
                e
            )
        })?;

        let sender = spawn_file_notifier(&tracked_dir)?;

        Ok(Arc::new(MessagingExeUnit {
            new_message_notifier: sender,
        }))
    }

    pub fn listen<MsgType: ExeUnitMessage + 'static>(&self) -> mpsc::UnboundedReceiver<MsgType> {
        let (msg_sender, msg_receiver) = mpsc::unbounded_channel();
        let mut file_receiver = self.new_message_notifier.subscribe();

        let future = async move {
            while let Ok(path) = file_receiver.recv().await {
                if let Some(content) = fs::read_to_string(&path)
                    .map_err(|e| {
                        log::warn!(
                            "[Messaging] Can't load msg from file: '{}'. {}",
                            &path.display(),
                            e
                        )
                    })
                    .ok()
                {
                    serde_json::from_slice::<MsgType>(&content.as_bytes())
                        .map_err(|e| {
                            log::warn!(
                                "Can't deserialize message from file '{}'. {}",
                                &path.display(),
                                e
                            )
                        })
                        .map(|msg| msg_sender.send(msg))
                        .ok();
                }
            }
        };
        tokio::spawn(future);
        return msg_receiver;
    }

    pub fn send(&self, msg: &impl ExeUnitMessage) -> anyhow::Result<()> {
        send_to_guest(msg)
    }
}

fn spawn_file_notifier(tracked_dir: &Path) -> anyhow::Result<broadcast::Sender<PathBuf>> {
    let (event_sender, _) = broadcast::channel(150);
    let sender = event_sender.clone();

    let (watcher_sender, watcher_receiver) = std::sync::mpsc::channel();
    let mut watcher =
        raw_watcher(watcher_sender).map_err(|e| anyhow!("Initializing watcher failed. {}", e))?;

    watcher
        .watch(&tracked_dir, RecursiveMode::NonRecursive)
        .map_err(|e| {
            anyhow!(
                "Starting watching directory '{}' failed. {}",
                &tracked_dir.display(),
                e
            )
        })?;

    std::thread::spawn(move || {
        // Take ownership of watcher.
        let _watcher = watcher;

        while let Ok(event) = watcher_receiver.recv() {
            match event.op {
                Ok(Op::CLOSE_WRITE) => {
                    let path = match event.path {
                        Some(path) => path,
                        None => continue,
                    };
                    event_sender.send(path).ok();
                }
                _ => (),
            }
        }
    });

    Ok(sender)
}
