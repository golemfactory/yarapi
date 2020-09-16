mod activity;
mod market;

pub use activity::{Activity, Credentials, ExeScriptCommand, RunningBatch};
pub use ya_client::web::{WebClient, WebClientBuilder};

pub use market::{Agreement, Market, Proposal, Subscription, SubscriptionId};

pub struct Session {
    client: WebClient,
}

impl Session {
    pub fn with_client(client: WebClient) -> Self {
        Session { client }
    }

    pub async fn create_activity(
        &self,
        agreement: &market::Agreement,
    ) -> anyhow::Result<impl activity::Activity> {
        activity::DefaultActivity::create(self.client.interface()?, agreement.id()).await
    }

    pub async fn create_secure_activity(
        &self,
        agreement: &market::Agreement,
    ) -> anyhow::Result<impl activity::Activity> {
        activity::SgxActivity::create(self.client.interface()?, agreement.id()).await
    }
}
