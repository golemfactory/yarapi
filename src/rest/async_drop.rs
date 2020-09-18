use crate::rest::async_drop::Command::DropAction;
use futures::channel::{mpsc, oneshot};
use futures::prelude::*;
use std::cell::RefCell;

#[derive(Clone)]
pub struct DropList(mpsc::UnboundedSender<Command>);

impl DropList {
    pub fn async_drop(&self, f: impl Future<Output = anyhow::Result<()>> + 'static) {
        log::debug!("async_drop added");
        let _ = self.0.unbounded_send(DropAction(
            async move {
                if let Err(e) = f.await {
                    log::error!(target: "yarapi::drop", "Error: {:?}", e);
                }
            }
            .boxed_local(),
        ));
    }

    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.0.unbounded_send(Command::Sync(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

impl Default for DropList {
    fn default() -> Self {
        drop_task()
    }
}

enum Command {
    DropAction(future::LocalBoxFuture<'static, ()>),
    Sync(oneshot::Sender<()>),
}

pub fn drop_task() -> DropList {
    let (tx, mut rx) = mpsc::unbounded();

    tokio::task::spawn_local(async move {
        log::debug!(target: "yarapi::drop", "Async drop started");
        while let Some(command) = rx.next().await {
            match command {
                Command::DropAction(f) => {
                    f.await;
                }
                Command::Sync(resp) => {
                    let _ = resp.send(());
                }
            }
        }
        log::debug!(target: "yarapi::drop", "Async drop terminated");
    });
    DropList(tx)
}

pub struct CancelableDropList(RefCell<Option<DropList>>);

#[allow(unused)]
impl CancelableDropList {
    pub fn new() -> Self {
        Self(RefCell::new(None))
    }

    pub fn cancel(&self) {
        let _ = self.take();
    }

    pub fn is_canceled(&self) -> bool {
        self.0.borrow().is_none()
    }

    pub fn take(&self) -> Option<DropList> {
        self.0.borrow_mut().take()
    }

    pub fn async_drop(&self, f: impl Future<Output = anyhow::Result<()>> + 'static) {
        if let Some(drop_list) = self.take() {
            drop_list.async_drop(f)
        } else {
            log::debug!("async_drop skipped");
        }
    }
}

impl From<DropList> for CancelableDropList {
    fn from(drop_list: DropList) -> Self {
        CancelableDropList(RefCell::new(Some(drop_list)))
    }
}
