#![allow(dead_code)]

use crate::requestor::command::{CommandList, ExeScript};
use anyhow::Result;
use ya_client::activity::ActivityRequestorApi;
use ya_client::model::activity::{ActivityState, ExeScriptCommandResult};

#[derive(Clone)]
pub(crate) struct Activity {
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
    ) -> Result<Self> {
        let activity_id = api.control().create_activity(&agreement_id).await?;
        Ok(Self {
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
        let batch_id = self
            .api
            .control()
            .exec(self.script.request.clone(), &self.activity_id)
            .await?;
        Ok(batch_id)
    }

    pub async fn get_exec_batch_results(
        &self,
        batch_id: &str,
    ) -> Result<Vec<ExeScriptCommandResult>> {
        let cmd_idx = Some(self.script.num_cmds - 1);
        let vec = self
            .api
            .control()
            .get_exec_batch_results(&self.activity_id, batch_id, None, cmd_idx)
            .await?;
        Ok(vec)
    }

    pub async fn get_state(&self) -> Result<ActivityState> {
        Ok(self.api.state().get_state(&self.activity_id).await?)
    }

    pub async fn get_usage(&self) -> Result<Vec<f64>> {
        Ok(self.api.state().get_usage(&self.activity_id).await?)
    }
}
