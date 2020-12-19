use anyhow::*;
use futures::prelude::*;
use futures::TryStreamExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::BufReader;
use std::path::Path;
use url::Url;

use crate::rest::activity::Event;
use crate::rest::{Activity, ExeScriptCommand, RunningBatch};

#[derive(thiserror::Error, Debug)]
pub enum FileTransferError {
    #[error(transparent)]
    Transfer(#[from] TransferCmdError),
    #[error("[{path}] is invalid URL. {e}")]
    BadUrl { path: String, e: String },
    #[error("Gftp error transferring from [{src}] to [{dest}]: {e}")]
    Gftp {
        src: String,
        dest: String,
        e: String,
    },
}

#[derive(thiserror::Error, Debug)]
pub enum TransferCmdError {
    #[error("Error transferring file from [{src}] to [{dest}]. Error: {e}")]
    CommandError {
        src: String,
        dest: String,
        e: String,
    },
}

#[derive(thiserror::Error, Debug)]
pub enum JsonTransferError {
    #[error("Error sending json. {0}")]
    Transfer(#[from] FileTransferError),
    #[error("Failed to serialize object to temp file. {0}")]
    Serialization(String),
    #[error("Failed to deserialize object from temp file. {0}")]
    Deserialization(String),
    #[error("Error sending json. {0}")]
    Internal(#[from] TransferInternalError),
}

#[derive(thiserror::Error, Debug)]
pub enum TransferInternalError {
    #[error("Failed to create temporary file. {0}")]
    CreatingTemporary(String),
    #[error("Can't persist temporary file. {0}")]
    PersistTemporary(String),
}

pub trait Transfers: Activity {
    fn send_file<'a>(
        &'a self,
        src: &Path,
        dest: &Path,
    ) -> future::LocalBoxFuture<'a, Result<(), FileTransferError>> {
        let src = src.to_path_buf();
        let dest = format!("container:{}", dest.display());

        async move {
            let dest = Url::parse(&dest).map_err(|e| FileTransferError::BadUrl {
                path: dest.clone(),
                e: e.to_string(),
            })?;

            let src = gftp::publish(&src)
                .await
                .map_err(|e| FileTransferError::Gftp {
                    src: src.to_string_lossy().to_string(),
                    dest: dest.to_string(),
                    e: e.to_string(),
                })?;

            self.transfer(&src, &dest)
                .await
                .map_err(FileTransferError::from)
        }
        .boxed_local()
    }

    fn send_json<'a, T: Serialize>(
        &'a self,
        dest: &Path,
        to_serialize: &'a T,
    ) -> future::LocalBoxFuture<'a, Result<(), JsonTransferError>> {
        let dest = dest.to_path_buf();
        async move {
            let (file, file_path) = tempfile::NamedTempFile::new()
                .map_err(|e| TransferInternalError::CreatingTemporary(e.to_string()))?
                .keep()
                .map_err(|e| TransferInternalError::PersistTemporary(e.to_string()))?;

            serde_json::to_writer(file, to_serialize)
                .map_err(|e| JsonTransferError::Serialization(e.to_string()))?;

            Ok(self.send_file(&file_path, &dest).await?)
        }
        .boxed_local()
    }

    fn download_file<'a>(
        &'a self,
        src: &Path,
        dest: &Path,
    ) -> future::LocalBoxFuture<'a, Result<(), FileTransferError>> {
        let dest = dest.to_path_buf();
        let src = format!("container:{}", src.display());

        async move {
            let src = Url::parse(&src).map_err(|e| FileTransferError::BadUrl {
                path: src.to_string(),
                e: e.to_string(),
            })?;

            let dest = gftp::open_for_upload(&dest)
                .await
                .map_err(|e| FileTransferError::Gftp {
                    src: src.to_string(),
                    dest: dest.to_string_lossy().to_string(),
                    e: e.to_string(),
                })?;

            Ok(self.transfer(&src, &dest).await?)
        }
        .boxed_local()
    }

    fn download_json<T: DeserializeOwned>(
        &self,
        src: &Path,
    ) -> future::LocalBoxFuture<Result<T, JsonTransferError>> {
        let src = src.to_path_buf();
        async move {
            let (file, file_path) = tempfile::NamedTempFile::new()
                .map_err(|e| TransferInternalError::CreatingTemporary(e.to_string()))?
                .keep()
                .map_err(|e| TransferInternalError::PersistTemporary(e.to_string()))?;

            self.download_file(&src, &file_path).await?;

            let reader = BufReader::new(file);
            serde_json::from_reader(reader)
                .map_err(|e| JsonTransferError::Deserialization(e.to_string()))
        }
        .boxed_local()
    }

    fn transfer<'a>(
        &'a self,
        src: &Url,
        dest: &Url,
    ) -> future::LocalBoxFuture<'a, Result<(), TransferCmdError>> {
        let src = src.to_string();
        let dest = dest.to_string();

        async move {
            let commands = vec![ExeScriptCommand::Transfer {
                from: src.clone(),
                to: dest.clone(),
                args: Default::default(),
            }];

            if let Err(e) = self.execute_commands(commands).await {
                let error = TransferCmdError::CommandError {
                    src: src.clone(),
                    dest: dest.clone(),
                    e: e.to_string(),
                };
                log::error!("{}", &error);
                return Err(error);
            }
            Ok(())
        }
        .boxed_local()
    }

    fn execute_commands(
        &self,
        commands: Vec<ExeScriptCommand>,
    ) -> future::LocalBoxFuture<anyhow::Result<Vec<String>>> {
        let batch = self.exec(commands);
        async move {
            batch
                .await?
                .events()
                .and_then(|event| match event {
                    Event::StepFailed { message } => {
                        future::err::<String, anyhow::Error>(anyhow!("Step failed: {}", message))
                    }
                    Event::StepSuccess { output, .. } => future::ok(output),
                })
                .try_collect()
                .await
        }
        .boxed_local()
    }
}

impl<T> Transfers for T where T: Activity {}
