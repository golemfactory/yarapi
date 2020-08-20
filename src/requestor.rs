use crate::{command::ExeScript, payment_manager, CommandList, Package};
use actix::prelude::*;
use anyhow::{Context, Result};
use bigdecimal::BigDecimal;
use futures::{channel::oneshot, future};
use payment_manager::PaymentManager;
use std::{
    collections::HashMap,
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

#[derive(Clone)]
pub enum Image {
    WebAssembly(semver::Version),
    GVMKit,
}

impl Image {
    pub fn runtime_name(&self) -> &'static str {
        match self {
            Image::WebAssembly(_) => "wasmtime",
            Image::GVMKit => "vm",
        }
    }

    pub fn runtime_version(&self) -> semver::Version {
        match self {
            Image::WebAssembly(version) => version.clone(),
            Image::GVMKit => semver::Version::new(0, 1, 0),
        }
    }
}

#[derive(Clone)]
pub struct Requestor {
    name: String,
    image_type: Image,
    task_package: Package,
    constraints: Constraints,
    tasks: Vec<CommandList>,
    timeout: Duration,
    budget: BigDecimal,
    status: String,
    on_completed: Option<Arc<dyn Fn(HashMap<String, String>)>>,
}

impl Requestor {
    /// Creates a new requestor from `Image` and `Package` with given `name`.
    pub fn new<T: Into<String>>(name: T, image_type: Image, task_package: Package) -> Self {
        Self {
            name: name.into(),
            image_type,
            task_package,
            constraints: constraints!["golem.com.pricing.model" == "linear"], /* TODO: other models */
            timeout: Duration::from_secs(60),
            tasks: vec![],
            budget: 0.into(),
            status: "".into(),
            on_completed: None,
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
    pub fn with_max_budget_gnt<T: Into<BigDecimal>>(self, budget: T) -> Self {
        Self {
            budget: budget.into(),
            ..self
        }
    }

    /// Adds tasks from the specified iterator.
    pub fn with_tasks(self, tasks: impl IntoIterator<Item = CommandList>) -> Self {
        Self {
            tasks: Vec::from_iter(tasks),
            ..self
        }
    }

    /// Sets callback to invoke upon completion of the tasks.
    pub fn on_completed<T: Fn(HashMap<String, String>) + 'static>(self, f: T) -> Self {
        Self {
            on_completed: Some(Arc::new(f)),
            ..self
        }
    }

    /// Runs all tasks asynchronously.
    pub async fn run(mut self) -> Result<()> {
        let app_key = std::env::var("YAGNA_APPKEY")?;

        let client = WebClient::builder().auth_token(&app_key).build();
        let market_api: MarketRequestorApi = client.interface()?;
        let activity_api: ActivityRequestorApi = client.interface()?;
        let payment_api: PaymentRequestorApi = client.interface()?;

        let providers_num = self.tasks.len();
        let demand = self.create_demand().await?;

        log::debug!("Demand: {}", serde_json::to_string(&demand)?);

        let subscription_id = market_api.subscribe(&demand).await?;

        log::info!("subscribed to Market API ( id : {} )", subscription_id);

        let allocation = payment_api
            .create_allocation(&model::payment::NewAllocation {
                address: None,
                payment_platform: None,
                total_amount: self.budget,
                timeout: None,
                make_deposit: false,
            })
            .await?;
        log::info!("allocated {} NGNT.", &allocation.total_amount);

        let payment_manager = PaymentManager::new(payment_api.clone(), allocation).start();

        #[derive(Debug, Copy, Clone, PartialEq)]
        enum ComputationState {
            WaitForInitialProposals,
            AnswerBestProposals,
            Done,
        }

        let mut state = ComputationState::WaitForInitialProposals;
        let mut proposals = vec![];
        let time_start = Instant::now();

        while state != ComputationState::Done {
            log::info!("getting new events, state: {:?}", state);

            let events = market_api
                .collect(&subscription_id, Some(2.0), Some(5))
                .await?;

            log::info!("received {} events", events.len());

            let mut futs = vec![];
            for e in events {
                match e {
                    RequestorEvent::ProposalEvent {
                        event_date: _,
                        proposal,
                    } => {
                        if proposal.state.unwrap_or(State::Initial) == State::Initial {
                            if proposal.prev_proposal_id.is_some() {
                                log::error!("proposal_id should be empty");
                                continue;
                            }

                            if state != ComputationState::WaitForInitialProposals {
                                /* ignore new proposals in other states */
                                continue;
                            }

                            log::info!("answering with counter proposal");

                            let bespoke_proposal = match proposal.counter_demand(demand.clone()) {
                                Ok(c) => c,
                                Err(e) => {
                                    log::error!("counter_demand error {}", e);
                                    continue;
                                }
                            };

                            let market_api_clone = market_api.clone();
                            let subscription_id_clone = subscription_id.clone();

                            let fut = spawn_job(|| async move {
                                market_api_clone
                                    .counter_proposal(&bespoke_proposal, &subscription_id_clone)
                                    .await
                            });

                            futs.push(fut);
                        } else {
                            proposals.push(proposal.clone());
                            log::debug!("got {} answer(s) to counter proposal", proposals.len());
                        }
                    }
                    _ => log::warn!("expected ProposalEvent"),
                }
            }

            // TODO we should handle any errors todo with counter proposal
            // submission. But is this the best place?
            future::try_join_all(futs).await?;

            /* check if there are enough proposals */
            if (time_start.elapsed() > Duration::from_secs(5)
                && proposals.len() >= 13 * providers_num / 10 + 2)
                || (time_start.elapsed() > Duration::from_secs(30)
                    && proposals.len() >= providers_num)
            {
                state = ComputationState::AnswerBestProposals;

                /* TODO choose only N best providers here */
                log::debug!("trying to sign agreements with providers");

                let mut futs = vec![];
                for i in 0..providers_num {
                    let market_api_clone = market_api.clone();
                    let activity_api_clone = activity_api.clone();
                    let payment_manager_clone = payment_manager.clone();

                    let proposal = proposals[i].clone();

                    let task = match self.tasks.pop() {
                        None => break,
                        Some(task) => task,
                    };
                    let exe_script = task.into_exe_script().await?;
                    log::info!("exe script: {:?}", exe_script);

                    let fut = spawn_job(|| async move {
                        Self::create_agreement(
                            market_api_clone,
                            activity_api_clone,
                            payment_manager_clone,
                            proposal,
                            exe_script,
                        )
                        .await
                    });
                    futs.push(fut);
                }

                proposals = vec![];
                let mut outputs = HashMap::new();
                for (prov_id, output) in future::try_join_all(futs).await? {
                    outputs.insert(prov_id, output);
                }

                log::info!("all activities finished");

                if let Some(fun) = self.on_completed.clone() {
                    fun(outputs);
                    state = ComputationState::Done;
                }

                loop {
                    let r = payment_manager.send(payment_manager::GetPending).await?;
                    log::info!("pending payments: {}", r);
                    if r <= 0 {
                        break;
                    }
                    tokio::time::delay_for(Duration::from_secs(1)).await;
                }
                // TODO payment_manager.send(payment_manager::ReleaseAllocation)
                // TODO market_api.unsubscribe(&subscription_id).await;
            }

            // TODO handle task timeout
            tokio::time::delay_until(tokio::time::Instant::now() + Duration::from_secs(3)).await;
        }

        log::info!("all tasks completed and paid for.");

        Ok(())
    }

    async fn create_demand(&self) -> Result<Demand> {
        // "golem.node.debug.subnet" == "mysubnet", TODO
        let (digest, url) = self.task_package.publish().await?;
        let url_with_hash = format!("hash:sha3:{}:{}", digest, url);
        let constraints = self.constraints.clone().and(constraints![
            "golem.runtime.name" == self.image_type.runtime_name(),
            "golem.runtime.version" == self.image_type.runtime_version().to_string(),
        ]);

        log::debug!("srv.comp.task_package: {}", url_with_hash);

        let demand = Demand::new(
            serde_json::json!({
                "golem": {
                    "node.id.name": self.name,
                    "srv.comp.task_package": url_with_hash,
                    "srv.comp.expiration":
                        (chrono::Utc::now() + chrono::Duration::minutes(10)).timestamp_millis(), // TODO
                },
            }),
            constraints.to_string(),
        );

        Ok(demand)
    }

    async fn create_agreement(
        market_api: MarketRequestorApi,
        activity_api: ActivityRequestorApi,
        payment_manager: Addr<PaymentManager>,
        proposal: Proposal,
        exe_script: ExeScript,
    ) -> Result<(String, String)> {
        let id = proposal.proposal_id()?;
        let issuer = proposal.issuer_id()?;
        log::debug!("hello issuer: {}", issuer);

        let agreement = AgreementProposal::new(
            id.clone(),
            chrono::Utc::now() + chrono::Duration::minutes(10), /* TODO */
        );

        log::info!("creating agreement");

        /* TODO handle errors */
        let r = market_api.create_agreement(&agreement).await;

        log::info!("create agreement result: {:?}; confirming", r);

        let _ = market_api.confirm_agreement(&id).await;

        log::info!("waiting for approval");

        let _ = market_api.wait_for_approval(&id, Some(10.0)).await;

        log::info!("approval received for agreement: {}", id);

        let activity_id = activity_api.control().create_activity(&id).await?;

        log::info!("activity created: {}", activity_id);

        let batch_id = activity_api
            .control()
            .exec(exe_script.request.clone(), &activity_id)
            .await
            .context("exec script failed!")?;

        let mut all_res = vec![];
        loop {
            log::info!("getting state of running activity {}", activity_id);

            let state = match activity_api.state().get_state(&activity_id).await {
                Ok(state) => state,
                Err(_) => break,
            };

            if !state.alive() {
                break;
            }

            all_res = activity_api
                .control()
                .get_exec_batch_results(
                    &activity_id,
                    &batch_id,
                    None,
                    Some(exe_script.num_cmds - 1),
                )
                .await?;
            log::debug!("batch_results: {}", all_res.len());

            if let Some(last) = all_res.last() {
                if last.is_batch_finished {
                    break;
                }
            }

            let delay = time::Instant::now() + Duration::from_secs(3);
            time::delay_until(delay).await;
        }

        if all_res.len() == exe_script.num_cmds
            && all_res
                .last()
                .map(|l| l.result == CommandResult::Ok)
                .unwrap_or(false)
        {
            log::info!("activity finished: {}", activity_id);
        } else {
            log::warn!("activity interrupted: {}", activity_id);
        }

        let only_stdout = |txt: String| {
            if txt.starts_with("stdout: ") {
                if let Some(pos) = txt.find("\nstderr:") {
                    &txt[8..pos]
                } else {
                    &txt[8..]
                }
            } else {
                ""
            }
            .to_string()
        };

        let output = all_res
            .into_iter()
            .enumerate()
            .filter_map(|(i, r)| match exe_script.run_indices.contains(&i) {
                // stdout: {}\nstdout;
                true => Some(r.message.unwrap_or("".to_string())).map(only_stdout),
                false => None,
            })
            .collect();

        let _ = payment_manager
            .send(payment_manager::AcceptAgreement {
                agreement_id: id.to_owned(),
            })
            .await?;

        // TODO not sure if this is not called too early
        activity_api
            .control()
            .destroy_activity(&activity_id)
            .await?;

        Ok((issuer.to_owned(), output))
    }
}

async fn spawn_job<T, F, R>(f: F) -> T
where
    F: FnOnce() -> R + 'static,
    R: Future<Output = T> + 'static,
    T: 'static,
{
    let (tx, rx) = oneshot::channel();
    Arbiter::spawn(async move {
        let res = f().await;
        let _ = tx.send(res);
    });
    rx.await.expect("oneshot should not fail")
}
