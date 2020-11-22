pub mod activity;
mod async_drop;
mod market;
pub mod streaming;

pub use activity::{Activity, Credentials, Event as BatchEvent, ExeScriptCommand, RunningBatch};
pub use ya_client::web::{WebClient, WebClientBuilder};

use futures::prelude::*;
pub use market::{Agreement, Market, Proposal, Subscription, SubscriptionId};

pub struct Session {
    client: WebClient,
    drop_list: async_drop::DropList,
}

impl Session {
    pub fn with_client(client: WebClient) -> Self {
        let drop_list = Default::default();
        Session { client, drop_list }
    }

    pub fn market(&self) -> anyhow::Result<Market> {
        Market::new(self.client.clone(), self.drop_list.clone())
    }

    pub async fn create_activity(
        &self,
        agreement: &market::Agreement,
    ) -> anyhow::Result<activity::DefaultActivity> {
        activity::DefaultActivity::create(
            self.client.interface()?,
            agreement.id(),
            Some(self.drop_list.clone()),
        )
        .await
    }

    pub async fn create_secure_activity(
        &self,
        agreement: &market::Agreement,
    ) -> anyhow::Result<activity::SgxActivity> {
        activity::SgxActivity::create(
            self.client.interface()?,
            agreement.id(),
            self.drop_list.clone().into(),
        )
        .await
    }

    pub async fn with<F: Future>(&self, work: F) -> Option<F::Output> {
        let result = {
            let ctrl_c = tokio::signal::ctrl_c();
            futures::pin_mut!(ctrl_c);
            let work = work;
            futures::pin_mut!(work);

            match future::select(work, ctrl_c).await {
                future::Either::Left((output, _)) => Some(output),
                future::Either::Right(_) => None,
            }
        };
        self.drop_list.flush().await;
        result
    }
}
