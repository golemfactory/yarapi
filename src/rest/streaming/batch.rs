use anyhow::Result;
use futures::prelude::*;
use futures::FutureExt;
use std::sync::Arc;

use crate::rest::activity::DefaultActivity;
use crate::rest::{Activity, RunningBatch};

use ya_client::activity::ActivityRequestorApi;
pub use ya_client::model::activity::Credentials;
pub use ya_client::model::activity::ExeScriptCommand;
use ya_client::model::activity::RuntimeEvent;

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

    pub async fn stream(&self) -> Result<impl Stream<Item = RuntimeEvent>> {
        Ok(self
            .api
            .control()
            .stream_exec_batch_results(&self.activity_id, &self.batch_id)
            .await?)
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
}
