use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, TimeZone, Utc};
use futures::prelude::*;
use futures::TryStreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::rest::async_drop::{CancelableDropList, DropList};
use std::collections::HashSet;
use std::fmt::Display;
use ya_client::market::MarketRequestorApi;
use ya_client::model::market::{NewDemand, AgreementEventType, AgreementOperationEvent, AgreementProposal, RequestorEvent,};
use ya_client::model::NodeId;
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
    pub api: MarketRequestorApi,
    drop_list: DropList,
}

impl Market {
    pub(crate) fn new(client: WebClient, drop_list: DropList) -> anyhow::Result<Self> {
        let api = client.interface()?;
        Ok(Self { api, drop_list })
    }

    pub async fn subscribe(
        &self,
        props: &serde_json::Value,
        constraints: &str,
    ) -> anyhow::Result<Subscription> {
        let demand = NewDemand::new(props.clone(), constraints.to_string());
        self.subscribe_demand(demand).await
    }

    pub async fn subscribe_demand(&self, demand: NewDemand) -> anyhow::Result<Subscription> {
        let subscription_id = self.api.subscribe(&demand).await?;
        Ok(Subscription::new(
            self.api.clone(),
            subscription_id.into(),
            self.drop_list.clone().into(),
        ))
    }

    pub async fn subscription(
        &self,
        subscription_id: SubscriptionId,
    ) -> anyhow::Result<Subscription> {
        Ok(Subscription::new(
            self.api.clone(),
            subscription_id,
            CancelableDropList::new(),
        ))
    }

    pub fn subscriptions(&self) -> impl Stream<Item = anyhow::Result<Subscription>> {
        stream::empty()
    }

    /// Lists all Agreements for identity, that is currently used.
    /// You can filter Agreements using AppSessionId parameter.
    pub async fn list_agreements<Tz>(
        &self,
        since: &DateTime<Tz>,
        app_session_id: Option<String>,
    ) -> anyhow::Result<Vec<String>>
    where
        Tz: TimeZone,
        Tz::Offset: Display,
    {
        Ok(self
            .list_agreement_events(since, app_session_id)
            .await?
            .into_iter()
            .filter_map(|event| match event.event_type {
                AgreementEventType::AgreementApprovedEvent => Some(event.agreement_id),
                _ => None,
            })
            .collect())
    }

    pub async fn list_active_agreements<Tz>(
        &self,
        since: &DateTime<Tz>,
        app_session_id: Option<String>,
    ) -> anyhow::Result<Vec<String>>
    where
        Tz: TimeZone,
        Tz::Offset: Display,
    {
        let mut agreements = HashSet::new();
        self.list_agreement_events(since, app_session_id)
            .await?
            .into_iter()
            .for_each(|event| {
                match event.event_type {
                    AgreementEventType::AgreementApprovedEvent => {
                        agreements.insert(event.agreement_id)
                    }
                    AgreementEventType::AgreementTerminatedEvent { .. } => {
                        agreements.remove(&event.agreement_id)
                    }
                    _ => false,
                };
                ()
            });

        Ok(agreements.into_iter().collect())
    }

    pub async fn list_agreement_events<Tz>(
        &self,
        since: &DateTime<Tz>,
        app_session_id: Option<String>,
    ) -> anyhow::Result<Vec<AgreementOperationEvent>>
    where
        Tz: TimeZone,
        Tz::Offset: Display,
    {
        let mut since = since.with_timezone(&Utc);
        let mut events = vec![];

        loop {
            let new_events = self
                .api
                .collect_agreement_events(Some(0.0), Some(&since), Some(30), app_session_id.clone())
                .await?;

            if new_events.is_empty() {
                return Ok(events);
            }

            since = new_events.last().unwrap().event_date;
            events.extend(new_events);
        }
    }
}

#[derive(Clone)]
pub struct Subscription {
    inner: Arc<SubscriptionInner>,
}

struct SubscriptionInner {
    id: SubscriptionId,
    api: MarketRequestorApi,
    drop_list: CancelableDropList,
}

impl Drop for SubscriptionInner {
    fn drop(&mut self) {
        let api = self.api.clone();
        let id = self.id.0.clone();
        self.drop_list.async_drop(async move {
            let _ = api.unsubscribe(&id).await?;
            log::debug!(target:"yarapi::drop", "Subscription {:?} destroyed", id);
            Ok(())
        });
    }
}

impl Subscription {
    fn new(api: MarketRequestorApi, id: SubscriptionId, drop_list: CancelableDropList) -> Self {
        let inner = Arc::new(SubscriptionInner { api, id, drop_list });
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
                        RequestorEvent::ProposalEvent { proposal, .. } => {
                            let subscription = subscription_iter.clone();
                            Some(Ok(Proposal {
                                subscription,
                                proposal_id: proposal.proposal_id.clone(),
                                data: proposal,
                            }))
                        }
                        _ => None,
                    })),
                    subscription,
                )))
            }
        })
        .try_flatten()
    }

    pub fn collect_proposals(&self) -> mpsc::Receiver<Proposal> {
        let (sender, receiver) = mpsc::channel(20);
        tokio::task::spawn_local(proposals_collector(self.inner.clone(), sender));
        receiver
    }

    /// TODO: We shouldn't pass Demand here, but we don't store initial Demand in subscription,
    ///       so we have no choice. Rethink this design.
    pub fn negotiated_proposals(&self, demand: NewDemand) -> mpsc::Receiver<Proposal> {
        let (mut sender, receiver) = mpsc::channel(20);
        let mut proposals = self.collect_proposals();

        tokio::task::spawn_local(async move {
            while let Some(proposal) = proposals.recv().await {
                if proposal.is_response() {
                    if let Err(_) = sender.send(proposal).await {
                        // Probably no one is listening for these events anymore.
                        return;
                    };
                } else {
                    proposal
                        .counter_proposal(&demand.properties, &demand.constraints)
                        .await
                        .map_err(|e| log::warn!("Failed to counter Proposal. Error: {}", e))
                        .ok();
                }
            }
        });
        receiver
    }

    pub async fn negotiate_agreements(
        &self,
        demand: NewDemand,
        num_agreements: usize,
        deadline: DateTime<Utc>,
    ) -> anyhow::Result<Vec<Agreement>> {
        let mut agreements = vec![];
        let mut proposals = self.negotiated_proposals(demand);

        while agreements.len() < num_agreements {
            if let Some(proposal) = proposals.recv().await {
                match negotiate_agreement(proposal, deadline).await {
                    Ok(agreement) => agreements.push(agreement),
                    Err(e) => log::warn!("Negotiating Agreement failed. {}", e),
                }
            }
        }

        Ok(agreements)
    }
}

pub async fn negotiate_agreement(
    proposal: Proposal,
    deadline: DateTime<Utc>,
) -> anyhow::Result<Agreement> {
    let agreement = proposal.create_agreement(deadline).await?;
    if let Err(e) = agreement.confirm().await {
        bail!("Waiting for approval failed. {}", e)
    }

    // TODO: Use AgreementView.
    let name = agreement
        .content()
        .await?
        .offer
        .properties
        .pointer("/golem.node.id.name")
        .map(|value| value.as_str().map(|name| name.to_string()))
        .flatten()
        .ok_or(anyhow!("Can't find node name in Agreement"))?;

    log::info!("Created agreement [{}] with '{}'", agreement.id(), name);
    return Ok(agreement);
}

async fn proposals_collector(
    subscription: Arc<SubscriptionInner>,
    mut sender: mpsc::Sender<Proposal>,
) {
    let id = subscription.id.clone();
    loop {
        let items = match subscription
            .api
            .collect(id.as_ref(), Some(30f32), Some(15i32))
            .await
        {
            Ok(items) => items,
            Err(e) => {
                log::debug!("Failed to collect proposals. Error: {}", e);
                continue;
            }
        };

        for item in items {
            match item {
                RequestorEvent::ProposalEvent { proposal, .. } => {
                    let proposal = Proposal {
                        subscription: subscription.clone(),
                        proposal_id: proposal.proposal_id.clone(),
                        data: proposal,
                    };

                    log::debug!(
                        "Got proposal: {} -- from: {}, state: {:?}",
                        proposal.id(),
                        proposal.issuer_id(),
                        proposal.state()
                    );

                    if let Err(_) = sender.send(proposal).await {
                        // Probably no one is listening for these events anymore.
                        return;
                    }
                }
                _ => continue,
            }
        }
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
        let proposal = ya_client::model::market::NewProposal {
            properties: props.clone(),
            constraints: constraints.to_string(),
        };
        Ok(self
            .subscription
            .api
            .counter_proposal(&proposal, self.subscription.id.as_ref(), &self.proposal_id)
            .await?)
    }

    pub fn state(&self) -> ya_client::model::market::proposal::State {
        self.data.state.clone()
    }

    pub fn is_response(&self) -> bool {
        self.data.prev_proposal_id.is_some()
    }

    pub async fn reject_proposal(&self) -> anyhow::Result<()> {
        let _ = self
            .subscription
            .api
            .reject_proposal(
                self.subscription.id.as_ref(),
                self.proposal_id.as_str(),
                &None,
            )
            .await?;
        Ok(())
    }

    pub async fn create_agreement(self, deadline: DateTime<Utc>) -> anyhow::Result<Agreement> {
        let ap = AgreementProposal {
            proposal_id: self.proposal_id,
            valid_to: deadline,
        };
        let agreement_id = self.subscription.api.create_agreement(&ap).await?;
        // TODO
        Ok(Agreement::new(
            self.subscription.api.clone(),
            agreement_id,
            CancelableDropList::new(),
        ))
    }

    pub fn props(&self) -> &serde_json::Value {
        &self.data.properties
    }

    pub fn issuer_id(&self) -> NodeId {
        self.data.issuer_id.clone()
    }
}

#[derive(Clone)]
pub struct Agreement {
    inner: Arc<AgreementInner>,
}

struct AgreementInner {
    agreement_id: String,
    api: MarketRequestorApi,
    drop_list: CancelableDropList,
}

impl Drop for AgreementInner {
    fn drop(&mut self) {
        let api = self.api.clone();
        let agreement_id = self.agreement_id.clone();
        self.drop_list.async_drop(async move {
            api.terminate_agreement(&agreement_id, &None)
                .await
                .with_context(|| format!("Failed to auto destroy Agreement: {:?}", agreement_id))?;
            log::debug!(target:"yarapi::drop", "Agreement {:?} terminated", agreement_id);
            Ok(())
        })
    }
}

impl Agreement {
    fn new(api: MarketRequestorApi, agreement_id: String, drop_list: CancelableDropList) -> Self {
        let inner = Arc::new(AgreementInner {
            api,
            agreement_id,
            drop_list,
        });
        Self { inner }
    }

    pub async fn confirm(&self) -> anyhow::Result<()> {
        let _ = self
            .inner
            .api
            .confirm_agreement(&self.inner.agreement_id, None)
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

    pub async fn content(&self) -> anyhow::Result<ya_client::model::market::Agreement> {
        Ok(self
            .inner
            .api
            .get_agreement(&self.inner.agreement_id)
            .await?)
    }

    pub fn id(&self) -> &str {
        &self.inner.agreement_id
    }
}
