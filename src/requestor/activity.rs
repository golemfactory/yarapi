#![allow(dead_code)]

use crate::requestor::command::{ExeScript, CommandList};
use anyhow::Result;
use ya_client::activity::{ActivityRequestorApi, SecureActivityRequestorApi};
use ya_client::model::activity::{ActivityState, ExeScriptCommandResult};

#[derive(Clone)]
enum ActivityKind {
    Default,
    Secure(SecureActivityRequestorApi),
}

#[derive(Clone)]
pub(crate) struct Activity {
    kind: ActivityKind,
    api: ActivityRequestorApi,
    pub agreement_id: String,
    pub activity_id: String,
    pub task: CommandList,
    pub script: ExeScript,
}

impl Activity {
    pub async fn create(
        api: ActivityRequestorApi,
        agreement_id: String,
        task: CommandList,
        secure: bool,
    ) -> Result<Self> {
        let (kind, activity_id) = if secure {
            let secure_api = api.control().create_secure_activity(&agreement_id).await?;
            let activity_id = secure_api.activity_id();
            (ActivityKind::Secure(secure_api), activity_id)
        } else {
            let activity_id = api.control().create_activity(&agreement_id).await?;
            (ActivityKind::Default, activity_id)
        };

        Ok(Activity {
            kind,
            api,
            agreement_id,
            activity_id,
            task: task.clone(),
            script: task.into_exe_script().await?,
        })
    }

    pub async fn destroy(&self) -> Result<()> {
        Ok(self
            .api
            .control()
            .destroy_activity(&self.activity_id)
            .await?)
    }

    pub async fn exec(&self) -> Result<String> {
        let batch_id = match &self.kind {
            ActivityKind::Default => {
                self.api
                    .control()
                    .exec(self.script.request.clone(), &self.activity_id)
                    .await?
            }
            ActivityKind::Secure(secure_api) => {
                let cmd_vec = serde_json::from_str(&self.script.request.text)?;
                secure_api.exec(cmd_vec).await?
            }
        };
        Ok(batch_id)
    }

    pub async fn get_exec_batch_results(
        &self,
        batch_id: &str,
    ) -> Result<Vec<ExeScriptCommandResult>> {
        let cmd_idx = Some(self.script.num_cmds - 1);
        let vec = match &self.kind {
            ActivityKind::Default => {
                self.api
                    .control()
                    .get_exec_batch_results(&self.activity_id, batch_id, None, cmd_idx)
                    .await?
            }
            ActivityKind::Secure(secure_api) => {
                secure_api
                    .get_exec_batch_results(batch_id, None, cmd_idx)
                    .await?
            }
        };
        Ok(vec)
    }

    pub async fn get_state(&self) -> Result<ActivityState> {
        Ok(self.api.state().get_state(&self.activity_id).await?)
    }

    pub async fn get_usage(&self) -> Result<Vec<f64>> {
        Ok(self.api.state().get_usage(&self.activity_id).await?)
    }
}
