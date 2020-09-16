use anyhow::{Context, Result};

use futures::future::LocalBoxFuture;
use futures::prelude::*;
use futures::stream::LocalBoxStream;
use futures::{FutureExt, StreamExt};
use std::sync::Arc;
use ya_client::activity::ActivityRequestorApi;
pub use ya_client::activity::SecureActivityRequestorApi;
pub use ya_client::model::activity::Credentials;
pub use ya_client::model::activity::ExeScriptCommand;
use ya_client::model::activity::ExeScriptRequest;
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

    fn events(&self) -> stream::LocalBoxStream<Result<Event>>;
}

pub(crate) struct DefaultActivity {
    api: ActivityRequestorApi,
    activity_id: String,
}

impl DefaultActivity {
    pub(crate) async fn create(api: ActivityRequestorApi, agreement_id: &str) -> Result<Self> {
        let activity_id = api
            .control()
            .create_activity(agreement_id)
            .await
            .with_context(|| {
                format!("failed to create activity for agreement {:?}", agreement_id)
            })?;
        Ok(Self { api, activity_id })
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
    api: ActivityRequestorApi,
    activity_id: String,
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
                .map(|r| r.index as usize)
                .max()
                .or(command_index);
            let is_last = result.iter().any(|r| r.is_batch_finished);
            let events = {
                let commands = commands.clone();
                result.into_iter().map(move |step| {
                    let command: &ExeScriptCommand =
                        commands.get(step.index as usize).ok_or_else(|| {
                            anyhow::anyhow!("invalid command response with index: {}", step.index)
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

    fn events(&self) -> stream::LocalBoxStream<'_, Result<Event>> {
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

pub(crate) struct SgxActivity {
    secure_api: SecureActivityRequestorApi,
    api: ActivityRequestorApi,
    activity_id: String,
}

impl SgxActivity {
    pub(crate) async fn create(api: ActivityRequestorApi, agreement_id: &str) -> Result<Self> {
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

pub(crate) struct SgxBatch {
    api: SecureActivityRequestorApi,
    batch_id: String,
    commands: Arc<[ExeScriptCommand]>,
}

impl RunningBatch for SgxBatch {
    fn id(&self) -> &str {
        &self.batch_id
    }

    fn events(&self) -> LocalBoxStream<'_, Result<Event>> {
        let api = self.api.clone();
        let batch_id: Arc<str> = self.batch_id.clone().into();

        generate_events(
            move |idx| {
                let api = api.clone();
                let batch_id = batch_id.clone();
                async move {
                    api.get_exec_batch_results(&batch_id, Some(30.0), idx)
                        .await
                        .with_context(|| {
                            format!("failed to fetch exec batch result for batch={:?}", batch_id)
                        })
                }
            },
            self.commands.clone(),
        )
        .boxed_local()
    }
}
