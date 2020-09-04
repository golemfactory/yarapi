mod activity;
mod command;
mod package;
mod payment_manager;

#[macro_use]
mod macros;

use actix::prelude::*;
use anyhow::{Context, Error, Result};
use bigdecimal::BigDecimal;
use futures::channel::mpsc;
use futures::future::{select, Either};
use futures::prelude::*;
use payment_manager::PaymentManager;
use std::{
    iter::FromIterator,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::time;
use ya_agreement_utils::{constraints, ConstraintKey, Constraints};
use ya_client::{
    activity::ActivityRequestorApi,
    market::MarketRequestorApi,
    model::{
        self,
        activity::CommandResult,
        market::{
            proposal::{Proposal, State},
            AgreementProposal, Demand, RequestorEvent,
        },
    },
    payment::PaymentRequestorApi,
    web::WebClient,
};

use crate::requestor::{activity::Activity, payment_manager::ReleaseAllocation};
pub use crate::requestor::{
    command::{Command, CommandList},
    package::{Image, Package},
};

const MAX_CONCURRENT_JOBS: usize = 64;

#[derive(Clone, Debug, MessageResponse)]
enum ComputationState {
    AwaitingProviders,
    AwaitingCompletion,
    Finished,
}

#[derive(Clone, Debug)]
struct ComputationTracker {
    initial: usize,
    completed: usize,
}

impl Default for ComputationTracker {
    fn default() -> Self {
        ComputationTracker {
            initial: 0,
            completed: 0,
        }
    }
}

#[derive(Clone)]
struct ProposalCtx {
    requestor: Addr<Requestor>,
    payment_manager: Addr<PaymentManager>,
    activity_api: ActivityRequestorApi,
    market_api: MarketRequestorApi,
}

#[derive(Clone)]
pub struct Requestor {
    name: String,
    subnet: String,
    image_type: Image,
    task_package: Package,
    constraints: Constraints,
    secure: bool,
    tasks: Vec<CommandList>,
    timeout: Duration,
    budget: BigDecimal,
    state: ComputationState,
    tracker: ComputationTracker,
    on_completed: Option<Arc<dyn Fn(String, Vec<String>)>>,
}

impl Requestor {
    /// Creates a new requestor from `Image` and `Package` with given `name`.
    pub fn new(name: impl Into<String>, image_type: Image, task_package: Package) -> Self {
        Self {
            name: name.into(),
            subnet: "testnet".into(),
            image_type,
            task_package,
            constraints: constraints!["golem.com.pricing.model" == "linear"], /* TODO: other models */
            secure: false,
            tasks: vec![],
            timeout: Duration::from_secs(300),
            budget: 0.into(),
            state: ComputationState::AwaitingProviders,
            tracker: ComputationTracker::default(),
            on_completed: None,
        }
    }

    /// Compute in a Trusted Execution Environment.
    pub fn secure(self) -> Self {
        Self {
            secure: true,
            ..self
        }
    }

    /// `Demand`s will be handled only by providers in this subnetwork.
    pub fn with_subnet(self, subnet: impl Into<String>) -> Self {
        Self {
            subnet: subnet.into(),
            ..self
        }
    }

    /// Adds `Constraints` for the specified tasks.
    pub fn with_constraints(self, constraints: Constraints) -> Self {
        Self {
            constraints: self.constraints.and(constraints),
            ..self
        }
    }

    /// Adds some `timeout` value for the tasks.
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self { timeout, ..self }
    }

    /// Sets the max budget in GNT.
    pub fn with_max_budget_ngnt<T: Into<BigDecimal>>(self, budget: T) -> Self {
        Self {
            budget: budget.into(),
            ..self
        }
    }

    /// Adds tasks from the specified iterator.
    pub fn with_tasks(mut self, tasks: impl IntoIterator<Item = CommandList>) -> Self {
        let tasks = Vec::from_iter(tasks);
        self.tracker.initial = tasks.len();
        Self { tasks, ..self }
    }

    /// Sets callback to invoke upon completion of the tasks.
    pub fn on_completed<T: Fn(String, Vec<String>) + 'static>(self, f: T) -> Self {
        Self {
            on_completed: Some(Arc::new(f)),
            ..self
        }
    }

    /// Runs all tasks asynchronously.
    pub async fn run(self) -> Result<()> {
        let app_key = std::env::var("YAGNA_APPKEY")?;

        let client = WebClient::builder().auth_token(&app_key).build();
        let market_api: MarketRequestorApi = client.interface()?;
        let activity_api: ActivityRequestorApi = client.interface()?;
        let payment_api: PaymentRequestorApi = client.interface()?;

        let demand = self.create_demand().await?;
        log::debug!("demand: {}", serde_json::to_string_pretty(&demand)?);

        let allocation = payment_api
            .create_allocation(&model::payment::NewAllocation {
                address: None,
                payment_platform: None,
                total_amount: self.budget.clone(),
                timeout: None,
                make_deposit: false,
            })
            .await?;
        log::info!("allocated {} NGNT", &allocation.total_amount);

        let subscription_id = market_api.subscribe(&demand).await?;
        log::info!("subscribed to market (id: [{}])", subscription_id);

        let secure = self.secure;
        let timeout = self.timeout;
        let payment_manager = PaymentManager::new(payment_api.clone(), allocation).start();
        let requestor = self.start();

        let (proposal_tx, proposal_rx) = mpsc::channel::<Proposal>(MAX_CONCURRENT_JOBS);
        let proposal_ctx = ProposalCtx {
            requestor: requestor.clone(),
            payment_manager: payment_manager.clone(),
            activity_api,
            market_api: market_api.clone(),
        };

        let compute = proposal_rx.for_each_concurrent(MAX_CONCURRENT_JOBS, move |proposal| {
            let ctx = proposal_ctx.clone();
            async move {
                let proposal_id = proposal.proposal_id.clone();
                let agreement_id = create_agreement(ctx.market_api.clone(), proposal)
                    .await
                    .with_context(|| {
                        format!("cannot create agreement for proposal [{:?}]", proposal_id)
                    })?;

                let task = async { Ok::<_, Error>(ctx.requestor.send(TakeTask).await??) }
                    .await
                    .with_context(|| format!("no tasks for agreement [{:?}]", agreement_id))?;

                let activity = Activity::create(
                    ctx.activity_api.clone(),
                    agreement_id.clone(),
                    task.clone(),
                    secure,
                )
                .await
                .with_context(|| {
                    format!("can't create activity for agreement [{:?}]", agreement_id)
                })?;

                let activity_id = activity.activity_id.clone();
                let task = activity.task.clone();
                let fut = monitor_activity(activity, ctx.payment_manager.clone()).then(
                    |result| async move {
                        match result {
                            Ok(o) => {
                                ctx.requestor.do_send(FinishTask(activity_id, o));
                            }
                            Err(e) => {
                                log::error!("activity [{}] error: {}", activity_id, e);
                                ctx.requestor.do_send(ReturnTask(task));
                            }
                        }
                    },
                );
                Arbiter::spawn(fut);

                Ok::<_, Error>(())
            }
            .map_err(|e| log::error!("activity error: {}", e))
            .then(|_| async move { () })
        });

        Arbiter::spawn(compute);
        Arbiter::spawn(process_market_events(
            requestor.clone(),
            market_api.clone(),
            subscription_id.clone(),
            demand,
            proposal_tx,
        ));

        match select(
            await_activity(requestor, timeout).boxed_local(),
            actix_rt::signal::ctrl_c().boxed_local(),
        )
        .await
        {
            Either::Left(_) => (),
            Either::Right((result, fut)) => match result {
                Ok(_) => log::warn!("interrupted with ctrl-c"),
                Err(_) => {
                    log::warn!("unable to bind a ctrl-c handler; waiting for computation");
                    fut.await;
                }
            },
        }

        log::info!("waiting for payments");
        loop {
            let r = payment_manager.send(payment_manager::GetPending).await?;
            if r <= 0 {
                break;
            }
            log::info!("pending payments: {}", r);
            tokio::time::delay_for(Duration::from_secs(1)).await;
        }

        log::info!("unsubscribing from the market");
        if let Err(e) = market_api.unsubscribe(&subscription_id).await {
            log::warn!("unable to unsubscribe from the market: {}", e);
        }

        log::info!("releasing allocation");
        if let Err(e) = payment_manager.send(ReleaseAllocation).await {
            log::warn!("unable to release allocation: {:?}", e);
        }

        Ok(())
    }

    async fn create_demand(&self) -> Result<Demand> {
        // "golem.node.debug.subnet" == "mysubnet", TODO
        let (digest, url) = self.task_package.publish().await?;
        let url_with_hash = format!("hash:sha3:{}:{}", digest, url);
        let constraints = self.constraints.clone().and(constraints![
            "golem.runtime.name" == self.image_type.runtime_name(),
            "golem.runtime.version" == self.image_type.runtime_version().to_string(),
            "golem.node.debug.subnet" == self.subnet.clone(),
        ]);

        log::debug!("srv.comp.task_package: {}", url_with_hash);

        let deadline = chrono::Utc::now() + chrono::Duration::from_std(self.timeout.clone())?;
        let demand = Demand::new(
            serde_json::json!({
                "golem.node.id.name": self.name,
                "golem.node.debug.subnet": self.subnet.clone(),
                "golem.srv.comp.task_package": url_with_hash,
                "golem.srv.comp.expiration": deadline.timestamp_millis(),
            }),
            constraints.to_string(),
        );

        Ok(demand)
    }
}

async fn process_market_events(
    requestor: Addr<Requestor>,
    market_api: MarketRequestorApi,
    subscription_id: String,
    demand: Demand,
    mut tx: mpsc::Sender<Proposal>,
) {
    log::info!("processing market events");
    'outer: loop {
        let events = market_api
            .collect(&subscription_id, Some(2.0), Some(5))
            .await
            .map_err(|e| log::error!("error collecting market events: {}", e))
            .unwrap_or_else(|_| Vec::new());
        log::debug!("collected {} market events", events.len());

        for event in events {
            match requestor.send(GetState).await {
                Ok(ComputationState::Finished) => break 'outer,
                Ok(ComputationState::AwaitingCompletion) => continue,
                Ok(ComputationState::AwaitingProviders) => (),
                Err(e) => {
                    log::error!("unable to read computation state: {:?}", e);
                    break 'outer;
                }
            }

            match event {
                RequestorEvent::ProposalEvent {
                    event_date: _,
                    proposal,
                } => match proposal.state.as_ref().unwrap_or(&State::Initial) {
                    State::Initial => {
                        log::debug!("answering with counter proposal");
                        let bespoke_proposal = match proposal.counter_demand(demand.clone()) {
                            Ok(c) => c,
                            Err(e) => {
                                log::error!("counter demand error {}", e);
                                continue;
                            }
                        };

                        let market_api_clone = market_api.clone();
                        let subscription_id_clone = subscription_id.clone();
                        Arbiter::spawn(async move {
                            if let Err(e) = market_api_clone
                                .counter_proposal(&bespoke_proposal, &subscription_id_clone)
                                .await
                            {
                                log::error!("unable to counter proposal: {}", e);
                            }
                        });
                    }
                    State::Draft => {
                        log::debug!("draft proposal from [{:?}]", proposal.issuer_id);
                        if let Err(e) = tx.send(proposal).await {
                            log::error!("unable to process proposal: {:?}", e);
                        }
                    }
                    state => {
                        log::debug!(
                            "ignoring proposal [{:?}] from [{:?}] with state {:?}",
                            proposal.proposal_id,
                            proposal.issuer_id,
                            state
                        );
                    }
                },
                _ => log::debug!("expected ProposalEvent"),
            }
        }
    }
    log::info!("stopped processing market events");
}

async fn create_agreement(market_api: MarketRequestorApi, proposal: Proposal) -> Result<String> {
    let id = proposal.proposal_id()?;
    let agreement = AgreementProposal::new(
        id.clone(),
        chrono::Utc::now() + chrono::Duration::minutes(10), /* TODO */
    );

    let agreement_id = market_api.create_agreement(&agreement).await?;
    log::info!(
        "created agreement [{}] with [{:?}]; confirming",
        agreement_id,
        proposal.issuer_id()
    );
    let _ = market_api.confirm_agreement(&id).await?;
    log::info!("waiting for approval of agreement [{}]", agreement_id);

    let response = market_api.wait_for_approval(&id, Some(10.0)).await?;
    match response.trim().to_lowercase().as_str() {
        "approved" => Ok(agreement_id),
        res => Err(anyhow::anyhow!(
            "expected agreement approval, got {} instead",
            res
        )),
    }
}

async fn monitor_activity(
    activity: Activity,
    payment_manager: Addr<PaymentManager>,
) -> Result<Vec<String>> {
    let _ = payment_manager
        .send(payment_manager::AcceptAgreement {
            agreement_id: activity.agreement_id.clone(),
        })
        .await?;

    let activity_id = activity.activity_id.clone();
    let batch_id = activity
        .exec()
        .await
        .map_err(|e| anyhow::anyhow!("exec failed: {}", e))?;

    let delay = Duration::from_secs(3);
    let mut results = vec![];
    loop {
        time::delay_for(delay).await;
        if !activity
            .get_state()
            .await
            .map_err(|e| anyhow::anyhow!("get_state failed: {}", e))?
            .alive()
        {
            log::warn!("activity [{}] is no longer alive", activity_id);
            break;
        };
        results = match activity.get_exec_batch_results(&batch_id).await {
            Ok(results) => results,
            Err(e) => match e.to_string().as_str() {
                "Timeout" => continue,
                _ => return Err(anyhow::anyhow!("get results error: {}", e)),
            },
        };
        if results.last().map(|r| r.is_batch_finished).unwrap_or(false) {
            log::info!("activity [{}] finished", activity_id);
            break;
        }
    }

    if results.len() != activity.script.num_cmds {
        log::warn!("activity [{}] interrupted", activity_id);
    } else if results
        .last()
        .map(|r| r.result != CommandResult::Ok)
        .unwrap_or(false)
    {
        log::warn!("activity [{}] failed", activity_id);
    }

    activity
        .destroy()
        .await
        .map_err(|e| anyhow::anyhow!("destroy failed: {}", e))?;

    let output = results
        .into_iter()
        .enumerate()
        .filter_map(|(i, r)| match activity.script.run_indices.contains(&i) {
            true => Some(r.message.unwrap_or_else(String::new)),
            false => None,
        })
        .collect::<Vec<_>>();

    Ok(output)
}

async fn await_activity(requestor: Addr<Requestor>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        match requestor.send(GetState).await {
            Ok(ComputationState::Finished) => {
                log::info!("all activities finished");
                break;
            }
            Err(e) => {
                log::error!("unable to retrieve state: internal error: {:?}", e);
                requestor.do_send(SetState(ComputationState::Finished));
                break;
            }
            _ => {
                if Instant::now() > deadline {
                    log::warn!("computation timed out after {:?}s", timeout.as_secs());
                    requestor.do_send(SetState(ComputationState::Finished));
                    break;
                }
            }
        }
        tokio::time::delay_for(Duration::from_secs(1)).await;
    }
}

impl Actor for Requestor {
    type Context = actix::Context<Self>;
}

#[derive(Message)]
#[rtype(result = "ComputationState")]
struct GetState;
actix_handler!(Requestor, GetState, |actor: &mut Requestor, _, _| {
    actor.state.clone()
});

#[derive(Message)]
#[rtype(result = "()")]
struct SetState(ComputationState);
actix_handler!(
    Requestor,
    SetState,
    |actor: &mut Requestor, msg: SetState, _| {
        actor.state = msg.0;
    }
);

#[derive(Message)]
#[rtype(result = "Result<CommandList>")]
struct TakeTask;
actix_handler!(Requestor, TakeTask, |actor: &mut Requestor, _, _| {
    match actor.tasks.pop() {
        Some(task) => {
            if actor.tasks.len() == 0 {
                actor.state = ComputationState::AwaitingCompletion;
            }
            Ok(task)
        }
        None => Err(anyhow::anyhow!("no more tasks")),
    }
});

#[derive(Message)]
#[rtype(result = "()")]
struct ReturnTask(CommandList);
actix_handler!(
    Requestor,
    ReturnTask,
    |actor: &mut Requestor, msg: ReturnTask, _| {
        actor.tasks.push(msg.0);
        actor.state = ComputationState::AwaitingProviders;
    }
);

#[derive(Message)]
#[rtype(result = "()")]
struct FinishTask(String, Vec<String>);
actix_handler!(
    Requestor,
    FinishTask,
    |actor: &mut Requestor, msg: FinishTask, _| {
        let track = &mut actor.tracker;
        track.completed += 1;

        log::info!(
            "completed {} tasks out of {}",
            track.completed,
            track.initial
        );

        if track.completed == track.initial {
            actor.state = ComputationState::Finished;
        }
        if let Some(f) = &actor.on_completed {
            f(msg.0, msg.1)
        }
    }
);
