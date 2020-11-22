use anyhow::Result;
use futures::prelude::*;
use futures::FutureExt;
use std::io::{self, Write};
use std::sync::Arc;

use crate::rest::activity::DefaultActivity;
use crate::rest::{Activity, RunningBatch};
use std::fs::File;
use std::path::Path;
use ya_client::activity::ActivityRequestorApi;
pub use ya_client::model::activity::Credentials;
pub use ya_client::model::activity::ExeScriptCommand;
use ya_client::model::activity::{CommandOutput, RuntimeEvent, RuntimeEventKind};

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
}

impl StreamingBatch {
    pub fn id(&self) -> &str {
        &self.batch_id
    }

    pub fn commands(&self) -> Vec<ExeScriptCommand> {
        self.commands.iter().cloned().collect()
    }

    /// Forwards stdout and stderr events to our stdout and stderr.
    /// Function doesn't consume events.
    /// TODO: This should be done through generic implementation for Stream<Item = RuntimeEvent>,
    ///  what would allow us to chain operations on events.
    pub async fn forward_std(&self) -> Result<impl Stream<Item = RuntimeEvent>> {
        Ok(self
            .api
            .control()
            .stream_exec_batch_results(&self.activity_id, &self.batch_id)
            .await?
            .map(|event| {
                match &event.kind {
                    RuntimeEventKind::StdOut(output) => write_to(&mut io::stdout(), &output).ok(),
                    RuntimeEventKind::StdErr(output) => write_to(&mut io::stderr(), &output).ok(),
                    _ => None,
                };
                event
            }))
    }

    pub async fn forward_to_file(
        &self,
        stdout: &Path,
        stderr: &Path,
    ) -> Result<impl Stream<Item = RuntimeEvent>> {
        let mut stdout_file = File::create(stdout)?;
        let mut stderr_file = File::create(stderr)?;

        Ok(self
            .api
            .control()
            .stream_exec_batch_results(&self.activity_id, &self.batch_id)
            .await?
            .map(move |event| {
                match &event.kind {
                    RuntimeEventKind::StdOut(output) => write_to(&mut stdout_file, &output).ok(),
                    RuntimeEventKind::StdErr(output) => write_to(&mut stderr_file, &output).ok(),
                    _ => None,
                };
                event
            }))
    }
}

fn write_to<OutType: Write>(stream: &mut OutType, output: &CommandOutput) -> anyhow::Result<()> {
    match output {
        CommandOutput::Bin(output) => stream.write(output.as_ref())?,
        CommandOutput::Str(output) => stream.write(output.as_ref())?,
    };
    Ok(())
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
}
