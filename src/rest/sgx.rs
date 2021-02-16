use anyhow::{Context, Result};
use futures::future::LocalBoxFuture;
use futures::stream::LocalBoxStream;
use futures::{FutureExt, StreamExt};
use std::sync::Arc;

use crate::rest::activity::{generate_events, Event};
use crate::rest::async_drop::CancelableDropList;
use crate::rest::{Activity, RunningBatch};

use ya_client::activity::ActivityRequestorApi;
pub use ya_client::activity::SecureActivityRequestorApi;
pub use ya_client::model::activity::Credentials;
pub use ya_client::model::activity::ExeScriptCommand;

pub struct SgxActivity {
    secure_api: SecureActivityRequestorApi,
    api: ActivityRequestorApi,
    activity_id: String,
    drop_list: CancelableDropList,
}

impl Drop for SgxActivity {
    fn drop(&mut self) {
        if let Some(ref drop_list) = self.drop_list.take() {
            let api = self.api.clone();
            let id = self.activity_id.clone();
            drop_list.async_drop(async move {
                api.control()
                    .destroy_activity(&id)
                    .await
                    .with_context(|| format!("Failed to auto destroy Activity: {:?}", id))?;
                log::debug!(target:"yarapi::drop", "Activity {:?} destroyed", id);
                Ok(())
            })
        }
    }
}

impl SgxActivity {
    pub(crate) async fn create(
        api: ActivityRequestorApi,
        agreement_id: &str,
        drop_list: CancelableDropList,
    ) -> Result<Self> {
        let secure_api = api
            .control()
            .create_secure_activity(agreement_id)
            .await
            .with_context(|| {
                format!("failed to create activity for agreement {:?}", agreement_id)
            })?;
        let activity_id = secure_api.activity_id();

        Ok(Self {
            api,
            secure_api,
            activity_id,
            drop_list,
        })
    }
}

impl Activity for SgxActivity {
    type RunningBatch = SgxBatch;

    fn id(&self) -> &str {
        &self.activity_id
    }

    fn exec(
        &self,
        commands: Vec<ExeScriptCommand>,
    ) -> LocalBoxFuture<'static, Result<Self::RunningBatch>> {
        let api = self.secure_api.clone();
        async move {
            let batch_commands = commands.clone().into();
            let batch_id = api.exec(commands).await?;
            Ok(SgxBatch {
                api,
                batch_id,
                commands: batch_commands,
            })
        }
        .boxed_local()
    }

    fn credentials(&self) -> Option<Credentials> {
        Some(self.secure_api.proof())
    }

    fn destroy(&self) -> LocalBoxFuture<'static, Result<()>> {
        let api = self.api.clone();
        let activity_id = self.activity_id.clone();
        async move {
            api.control()
                .destroy_activity(&activity_id)
                .await
                .with_context(|| format!("failed to destroy sgx activity: {:?}", activity_id))
        }
        .boxed_local()
    }
}

pub struct SgxBatch {
    api: SecureActivityRequestorApi,
    batch_id: String,
    commands: Arc<[ExeScriptCommand]>,
}

impl RunningBatch for SgxBatch {
    fn id(&self) -> &str {
        &self.batch_id
    }

    fn commands(&self) -> Vec<ExeScriptCommand> {
        self.commands.iter().cloned().collect()
    }

    fn events(&self) -> LocalBoxStream<'static, Result<Event>> {
        let api = self.api.clone();
        let batch_id: Arc<str> = self.batch_id.clone().into();

        generate_events(
            move |idx| {
                let api = api.clone();
                let batch_id = batch_id.clone();
                async move {
                    loop {
                        match api.get_exec_batch_results(&batch_id, Some(10.0), idx).await {
                            Ok(v) => return Ok(v),
                            Err(ya_client::Error::TimeoutError { .. }) => (),
                            Err(ya_client::Error::InternalError(ref msg)) if msg == "Timeout" => (),
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
            },
            self.commands.clone(),
        )
        .boxed_local()
    }
}
