use anyhow::Context;
use chrono::{DateTime, Utc};
use futures::prelude::*;
use futures::TryStreamExt;
use std::sync::Arc;

use ya_client::market::MarketRequestorApi;
use ya_client::model::market::proposal::State;
use ya_client::model::market::Demand;
use ya_client::model::market::{AgreementProposal, RequestorEvent};
use ya_client::web::WebClient;

#[derive(Clone)]
pub struct SubscriptionId(String);

impl From<String> for SubscriptionId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl AsRef<str> for SubscriptionId {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

pub struct Market {
    api: MarketRequestorApi,
}

impl Market {
    pub fn new(client: WebClient) -> anyhow::Result<Self> {
        let api = client.interface()?;
        Ok(Self { api })
    }

    pub async fn subscribe(
        &self,
        props: &serde_json::Value,
        constraints: &str,
    ) -> anyhow::Result<Subscription> {
        let demand = Demand::new(props.clone(), constraints.to_string());

        let subscription_id = self.api.subscribe(&demand).await?;
        Ok(Subscription::new(self.api.clone(), subscription_id.into()))
    }

    pub async fn subscription(
        &self,
        subscription_id: SubscriptionId,
    ) -> anyhow::Result<Subscription> {
        Ok(Subscription::new(self.api.clone(), subscription_id))
    }

    pub fn subscriptions(&self) -> impl Stream<Item = anyhow::Result<Subscription>> {
        stream::empty()
    }
}

#[derive(Clone)]
pub struct Subscription {
    inner: Arc<SubscriptionInner>,
}

struct SubscriptionInner {
    id: SubscriptionId,
    api: MarketRequestorApi,
}

impl Subscription {
    fn new(api: MarketRequestorApi, id: SubscriptionId) -> Self {
        let inner = Arc::new(SubscriptionInner { api, id });
        Subscription { inner }
    }

    pub fn id(&self) -> &SubscriptionId {
        &self.inner.id
    }

    pub fn proposals(&self) -> impl Stream<Item = anyhow::Result<Proposal>> {
        stream::try_unfold(self.inner.clone(), move |subscription| async move {
            let items = subscription
                .api
                .collect(subscription.id.as_ref(), Some(30f32), Some(15i32))
                .await?;
            {
                let subscription_iter = subscription.clone();
                Ok::<_, anyhow::Error>(Some((
                    stream::iter(items.into_iter().filter_map(move |event| match event {
                        RequestorEvent::ProposalEvent { mut proposal, .. } => {
                            let subscription = subscription_iter.clone();
                            if let Some(proposal_id) = proposal.proposal_id.take() {
                                Some(Ok(Proposal {
                                    subscription,
                                    proposal_id,
                                    data: proposal,
                                }))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    })),
                    subscription,
                )))
            }
        })
        .try_flatten()
    }
}

pub struct Proposal {
    subscription: Arc<SubscriptionInner>,
    proposal_id: String,
    data: ya_client::model::market::Proposal,
}

impl Proposal {
    pub fn id(&self) -> &str {
        &self.proposal_id
    }

    pub async fn counter_proposal(
        &self,
        props: &serde_json::Value,
        constraints: &str,
    ) -> anyhow::Result<String> {
        let proposal = ya_client::model::market::Proposal {
            properties: props.clone(),
            constraints: constraints.to_string(),
            proposal_id: None,
            issuer_id: None,
            state: None,
            prev_proposal_id: Some(self.proposal_id.clone()),
        };
        Ok(self
            .subscription
            .api
            .counter_proposal(&proposal, self.subscription.id.as_ref())
            .await?)
    }

    pub fn state(&self) -> ya_client::model::market::proposal::State {
        self.data.state.unwrap_or_else(|| State::Initial)
    }

    pub fn is_response(&self) -> bool {
        self.data.prev_proposal_id.is_some()
    }

    pub async fn reject_proposal(&self) -> anyhow::Result<()> {
        let _ = self
            .subscription
            .api
            .reject_proposal(self.subscription.id.as_ref(), self.proposal_id.as_str())
            .await?;
        Ok(())
    }

    pub async fn create_agreement(self, deadline: DateTime<Utc>) -> anyhow::Result<Agreement> {
        let ap = AgreementProposal {
            proposal_id: self.proposal_id,
            valid_to: deadline,
        };
        let agreement_id = self.subscription.api.create_agreement(&ap).await?;
        Ok(Agreement::new(self.subscription.api.clone(), agreement_id))
    }

    pub fn props(&self) -> &serde_json::Value {
        &self.data.properties
    }

    pub fn issuer_id(&self) -> &str {
        self.data
            .issuer_id
            .as_ref()
            .map(AsRef::as_ref)
            .unwrap_or_default()
    }
}

#[derive(Clone)]
pub struct Agreement {
    inner: Arc<AgreementInner>,
}

struct AgreementInner {
    agreement_id: String,
    api: MarketRequestorApi,
}

impl Agreement {
    fn new(api: MarketRequestorApi, agreement_id: String) -> Self {
        let inner = Arc::new(AgreementInner { api, agreement_id });
        Self { inner }
    }

    pub async fn confirm(&self) -> anyhow::Result<()> {
        let _ = self
            .inner
            .api
            .confirm_agreement(&self.inner.agreement_id)
            .await
            .with_context(|| {
                format!(
                    "failed to confirm_agreement agreement_id={}",
                    self.inner.agreement_id
                )
            })?;
        let _ = self
            .inner
            .api
            .wait_for_approval(&self.inner.agreement_id, Some(15.0))
            .await
            .with_context(|| {
                format!(
                    "error while wait_for_approval agreement_id={}",
                    self.inner.agreement_id
                )
            })?;

        Ok(())
    }

    pub fn id(&self) -> &str {
        &self.inner.agreement_id
    }
}

#[cfg(test)]
mod test {

    #[test]
    fn test_build() {}
}
