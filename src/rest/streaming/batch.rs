use anyhow::{anyhow, Result};
use futures::prelude::*;
use futures::FutureExt;
use std::sync::Arc;

use crate::rest::activity::DefaultActivity;
use crate::rest::{Activity, RunningBatch};

use ya_client::activity::ActivityRequestorApi;
use ya_client::model::activity::{Capture, CaptureFormat, CaptureMode};
use ya_client::model::activity::{CommandResult, RuntimeEvent};
pub use ya_client::model::activity::{Credentials, ExeScriptCommand};

pub struct StreamingBatch {
    api: ActivityRequestorApi,
    activity_id: String,
    batch_id: String,
    commands: Arc<[ExeScriptCommand]>,
}

pub trait StreamingActivity {
    fn exec_streaming(
        &self,
        commands: Vec<ExeScriptCommand>,
    ) -> future::LocalBoxFuture<'static, Result<StreamingBatch>>;

    fn run_streaming(
        &self,
        entry_point: &str,
        args: Vec<String>,
    ) -> future::LocalBoxFuture<'static, Result<StreamingBatch>>;
}

impl StreamingBatch {
    pub fn id(&self) -> &str {
        &self.batch_id
    }

    pub fn commands(&self) -> Vec<ExeScriptCommand> {
        self.commands.iter().cloned().collect()
    }

    pub async fn stream(&self) -> Result<impl Stream<Item = RuntimeEvent>> {
        Ok(self
            .api
            .control()
            .stream_exec_batch_results(&self.activity_id, &self.batch_id)
            .await?)
    }

    pub async fn wait_for_finish(&self) -> anyhow::Result<()> {
        let last = self.commands.len() - 1;

        loop {
            let results = self
                .api
                .control()
                .get_exec_batch_results(&self.activity_id, &self.batch_id, Some(30.0), Some(last))
                .await?;
            if !results.is_empty() {
                let last_result = results.last().unwrap();
                if last_result.is_batch_finished {
                    return match last_result.result {
                        CommandResult::Ok => Ok(()),
                        CommandResult::Error => Err(anyhow!(last_result
                            .message
                            .clone()
                            .unwrap_or("".to_string()))),
                    };
                }
            }
        }
    }
}

impl StreamingActivity for DefaultActivity {
    fn exec_streaming(
        &self,
        commands: Vec<ExeScriptCommand>,
    ) -> future::LocalBoxFuture<'static, Result<StreamingBatch>> {
        let batch_fut = self.exec(commands);
        async move {
            batch_fut.await.map(|batch| StreamingBatch {
                batch_id: batch.id().to_string(),
                commands: Arc::from(batch.commands()),
                api: batch.api,
                activity_id: batch.activity_id,
            })
        }
        .boxed_local()
    }

    fn run_streaming(
        &self,
        entry_point: &str,
        args: Vec<String>,
    ) -> future::LocalBoxFuture<'static, Result<StreamingBatch>> {
        let capture = Some(CaptureMode::Stream {
            limit: None,
            format: Some(CaptureFormat::Str),
        });

        let commands = vec![ExeScriptCommand::Run {
            entry_point: entry_point.to_string(),
            args: args.clone(),
            capture: Some(Capture {
                stdout: capture.clone(),
                stderr: capture,
            }),
        }];

        self.exec_streaming(commands)
    }
}
