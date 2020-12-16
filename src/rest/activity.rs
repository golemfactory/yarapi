use anyhow::{anyhow, Context, Result};
use futures::future::LocalBoxFuture;
use futures::prelude::*;
use futures::stream::LocalBoxStream;
use futures::{FutureExt, StreamExt};
use std::sync::Arc;

use crate::rest::async_drop::{CancelableDropList, DropList};
use ya_client::activity::ActivityRequestorApi;
pub use ya_client::activity::SecureActivityRequestorApi;
pub use ya_client::model::activity::Credentials;
pub use ya_client::model::activity::ExeScriptCommand;
use ya_client::model::activity::{ActivityState, ExeScriptCommandState, ExeScriptRequest};
use ya_client::model::activity::{CommandResult, ExeScriptCommandResult};

#[derive(Debug)]
pub enum Event {
    StepSuccess {
        command: ExeScriptCommand,
        output: String,
    },
    StepFailed {
        message: String,
    },
}

pub trait Activity {
    type RunningBatch: RunningBatch;

    fn id(&self) -> &str;

    fn exec(
        &self,
        commands: Vec<ExeScriptCommand>,
    ) -> future::LocalBoxFuture<'static, Result<Self::RunningBatch>>;

    fn credentials(&self) -> Option<Credentials>;

    fn destroy(&self) -> future::LocalBoxFuture<'static, Result<()>>;
}

pub trait RunningBatch {
    fn id(&self) -> &str;
    fn commands(&self) -> Vec<ExeScriptCommand>;

    fn events(&self) -> stream::LocalBoxStream<'static, Result<Event>>;
}

pub struct DefaultActivity {
    pub(crate) api: ActivityRequestorApi,
    activity_id: String,
    drop_list: Option<DropList>,
}

impl DefaultActivity {
    pub(crate) async fn create(
        api: ActivityRequestorApi,
        agreement_id: &str,
        drop_list: Option<DropList>,
    ) -> Result<Self> {
        let activity_id = api
            .control()
            .create_activity(agreement_id)
            .await
            .with_context(|| {
                format!("failed to create activity for agreement {:?}", agreement_id)
            })?;
        Ok(Self {
            api,
            activity_id,
            drop_list,
        })
    }

    /// Debug function, that allows to attach to existing Activity.
    pub(crate) fn attach_to_activity(
        api: ActivityRequestorApi,
        activity_id: &str,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            api,
            activity_id: activity_id.to_string(),
            drop_list: None,
        })
    }

    /// Debug function to attach to existing batch.
    pub fn attach_to_batch(&self, batch_id: &str) -> DefaultBatch {
        DefaultBatch {
            batch_id: batch_id.to_string(),
            commands: Arc::from(vec![]),
            api: self.api.clone(),
            activity_id: self.activity_id.clone(),
        }
    }

    pub async fn execute_commands(
        &self,
        commands: Vec<ExeScriptCommand>,
    ) -> anyhow::Result<Vec<String>> {
        let batch = self.exec(commands).await?;
        batch
            .events()
            .and_then(|event| {
                log::debug!("Event: {:?}", event);
                match event {
                    Event::StepFailed { message } => {
                        future::err::<String, anyhow::Error>(anyhow!("Step failed: {}", message))
                    }
                    Event::StepSuccess { command, output } => {
                        log::debug!("Command [{:?}] finished.", command);
                        log::debug!("Command result:\n {}", output);
                        future::ok(output)
                    }
                }
            })
            .try_collect()
            .await
    }

    pub async fn get_state(&self) -> anyhow::Result<ActivityState> {
        Ok(self
            .api
            .state()
            .get_state(&self.activity_id)
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to get state for activity [{}]. {}",
                    &self.activity_id,
                    e
                )
            })?)
    }

    pub async fn get_running_command(&self) -> anyhow::Result<ExeScriptCommandState> {
        Ok(self
            .api
            .state()
            .get_running_command(&self.activity_id)
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to get running command for activity [{}]. {}",
                    &self.activity_id,
                    e
                )
            })?)
    }

    pub async fn get_usage(&self) -> anyhow::Result<Vec<f64>> {
        Ok(self
            .api
            .state()
            .get_usage(&self.activity_id)
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to get usage for activity [{}]. {}",
                    &self.activity_id,
                    e
                )
            })?)
    }
}

impl Drop for DefaultActivity {
    fn drop(&mut self) {
        if let Some(ref drop_list) = self.drop_list {
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

impl Activity for DefaultActivity {
    type RunningBatch = DefaultBatch;

    fn id(&self) -> &str {
        &self.activity_id
    }

    fn exec(
        &self,
        commands: Vec<ExeScriptCommand>,
    ) -> future::LocalBoxFuture<'static, Result<Self::RunningBatch>> {
        let api = self.api.clone();
        let activity_id = self.activity_id.clone();

        async move {
            let request = ExeScriptRequest {
                text: serde_json::to_string(&commands)?,
            };

            let batch_id = api.control().exec(request, &activity_id).await?;

            Ok(DefaultBatch {
                api,
                activity_id,
                batch_id,
                commands: commands.into(),
            })
        }
        .boxed_local()
    }

    fn credentials(&self) -> Option<Credentials> {
        None
    }

    fn destroy(&self) -> LocalBoxFuture<'static, Result<()>> {
        let api = self.api.clone();
        let activity_id = self.activity_id.clone();
        async move {
            api.control()
                .destroy_activity(&activity_id)
                .await
                .with_context(|| format!("failed to destroy activity: {:?}", activity_id))
        }
        .boxed_local()
    }
}

pub struct DefaultBatch {
    pub(crate) api: ActivityRequestorApi,
    pub(crate) activity_id: String,
    batch_id: String,
    commands: Arc<[ExeScriptCommand]>,
}

fn generate_events<Generator, GResult>(
    generator: Generator,
    commands: Arc<[ExeScriptCommand]>,
) -> impl Stream<Item = Result<Event>>
where
    Generator: FnMut(Option<usize>) -> GResult,
    GResult: Future<Output = Result<Vec<ExeScriptCommandResult>>>,
{
    stream::try_unfold(
        (generator, commands, None, false),
        |(mut generator, commands, command_index, finish)| async move {
            if finish {
                return Ok(None);
            }
            let result = generator(command_index).await?;

            let last_index = result
                .iter()
                .map(|r| (r.index + 1) as usize)
                .max()
                .or(command_index);
            let is_last = result.iter().any(|r| r.is_batch_finished);
            let events = {
                let commands = commands.clone();
                result
                    .into_iter()
                    .filter(move |s| Some(s.index as usize) >= command_index)
                    .map(move |step| {
                        let command: &ExeScriptCommand =
                            commands.get(step.index as usize).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "invalid command response with index: {}",
                                    step.index
                                )
                            })?;
                        match step.result {
                            CommandResult::Ok => Ok(Event::StepSuccess {
                                command: command.clone(),
                                output: step.message.unwrap_or_default(),
                            }),
                            CommandResult::Error => Ok(Event::StepFailed {
                                message: step.message.unwrap_or_default(),
                            }),
                        }
                    })
            };

            Ok::<_, anyhow::Error>(Some((
                stream::iter(events),
                (generator, commands, last_index, is_last),
            )))
        },
    )
    .try_flatten()
}

impl RunningBatch for DefaultBatch {
    fn id(&self) -> &str {
        &self.batch_id
    }

    fn commands(&self) -> Vec<ExeScriptCommand> {
        self.commands.iter().cloned().collect()
    }

    fn events(&self) -> stream::LocalBoxStream<'static, Result<Event>> {
        let commands = self.commands.clone();
        let api = self.api.clone();
        let activity_id = self.activity_id.clone();
        let batch_id = self.batch_id.clone();

        generate_events(
            move |command_index| {
                let api = api.clone();
                let activity_id = activity_id.clone();
                let batch_id = batch_id.clone();

                async move {
                    Ok(api
                        .control()
                        .get_exec_batch_results(&activity_id, &batch_id, Some(30.0), command_index)
                        .await?)
                }
            },
            commands,
        )
        .boxed_local()
    }
}

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
