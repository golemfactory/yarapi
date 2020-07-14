use crate::{payment_manager, CommandList, Package};
use actix::prelude::*;
use anyhow::Result;
use bigdecimal::BigDecimal;
use futures::{SinkExt, StreamExt};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use ya_agreement_utils::{constraints, ConstraintKey, Constraints};
use ya_client::{
    activity::ActivityRequestorApi,
    market::MarketRequestorApi,
    model::{
        self,
        market::{proposal::State, AgreementProposal, Demand, RequestorEvent},
    },
    payment::PaymentRequestorApi,
};

#[derive(Clone)]
pub enum Image {
    WebAssembly(semver::Version),
    GVMKit,
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
    on_completed: Option<Arc<dyn Fn(Vec<String>)>>,
}

impl Requestor {
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
    pub fn with_constraints(self, constraints: Constraints) -> Self {
        Self {
            constraints: constraints.clone().and(constraints),
            ..self
        }
    }
    pub fn with_timeout(self, timeout: std::time::Duration) -> Self {
        Self { timeout, ..self }
    }
    pub fn with_max_budget_gnt<T: Into<BigDecimal>>(self, budget: T) -> Self {
        Self {
            budget: budget.into(),
            ..self
        }
    }
    pub fn with_tasks<T: std::iter::Iterator<Item = CommandList>>(self, tasks: T) -> Self {
        let tasks_vec: Vec<CommandList> = tasks.collect();
        //let n = tasks_vec.len();
        Self {
            tasks: tasks_vec,
            //stdout_results: vec!["".to_string(); n],
            ..self
        }
    }
    pub fn on_completed<T: Fn(Vec<String>) + 'static>(self, f: T) -> Self {
        Self {
            on_completed: Some(Arc::new(f)),
            ..self
        }
    }

    pub async fn run(mut self) -> Result<()> {
        let app_key = std::env::var("YAGNA_APPKEY").unwrap();
        let client = ya_client::web::WebClient::builder()
            .auth_token(&app_key)
            .build();
        let market_api: MarketRequestorApi = client.interface()?;
        let activity_api: ActivityRequestorApi = client.interface()?;
        let payment_api: PaymentRequestorApi = client.interface()?;
        //let timeout = self.timeout;
        let providers_num = self.tasks.len();
        let demand = self.create_demand().await;

        log::info!("Demand: {}", serde_json::to_string(&demand).unwrap());

        let subscription_id = market_api.subscribe(&demand).await?;

        log::info!("subscribed to Market API ( id : {} )", subscription_id);

        let allocation = payment_api
            .create_allocation(&model::payment::NewAllocation {
                total_amount: self.budget,
                timeout: None,
                make_deposit: false,
            })
            .await?;
        log::info!("allocated {} GNT.", &allocation.total_amount);

        let payment_manager =
            payment_manager::PaymentManager::new(payment_api.clone(), allocation).start();

        #[derive(Copy, Clone, PartialEq)]
        enum ComputationState {
            WaitForInitialProposals,
            AnswerBestProposals,
            Done,
        }
        let mut state = ComputationState::WaitForInitialProposals;
        let mut proposals = vec![];
        let time_start = Instant::now();
        while state != ComputationState::Done {
            log::info!("getting new events, state: {}", state as u8);
            let events = market_api
                .collect(&subscription_id, Some(2.0), Some(5))
                .await?;
            log::info!("received {} events", events.len());
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

                            Arbiter::spawn(async move {
                                let _ = market_api_clone
                                    .counter_proposal(&bespoke_proposal, &subscription_id_clone)
                                    .await;
                            });
                        } else {
                            proposals.push(proposal.clone());
                            log::debug!("got {} answer(s) to counter proposal", proposals.len());
                        }
                    }
                    _ => log::warn!("expected ProposalEvent"),
                }
            }
            /* check if there are enough proposals */
            if (time_start.elapsed() > Duration::from_secs(5)
                && proposals.len() >= 13 * providers_num / 10 + 2)
                || (time_start.elapsed() > Duration::from_secs(30)
                    && proposals.len() >= providers_num)
            {
                let (output_tx, output_rx) = futures::channel::mpsc::unbounded::<(usize, String)>();
                state = ComputationState::AnswerBestProposals;
                /* TODO choose only N best providers here */
                log::debug!("trying to sign agreements with providers");

                for i in 0..providers_num {
                    let pr = &proposals[i];
                    let market_api_clone = market_api.clone();
                    let activity_api_clone = activity_api.clone();
                    let agr_id = pr.proposal_id().unwrap().clone();
                    let issuer = pr.issuer_id().unwrap().clone();
                    log::debug!("hello issuer: {}", issuer);

                    let task = match self.tasks.pop() {
                        None => break,
                        Some(task) => task,
                    };
                    let (script, num_cmds, run_ind) = task.into_exe_script().await?;
                    log::info!("exe script: {:?}", script);

                    let mut output_tx_clone = output_tx.clone();
                    let payment_manager_clone = payment_manager.clone();
                    Arbiter::spawn(async move {
                        log::debug!("issuer: {}", issuer);
                        let agr = AgreementProposal::new(
                            agr_id.clone(),
                            chrono::Utc::now() + chrono::Duration::minutes(10), /* TODO */
                        );
                        log::info!("creating agreement");
                        /* TODO handle errors */
                        let r = market_api_clone.create_agreement(&agr).await;
                        log::info!("create agreement result: {:?}; confirming", r);
                        let _ = market_api_clone.confirm_agreement(&agr_id).await;
                        log::info!("waiting for approval");
                        let _ = market_api_clone
                            .wait_for_approval(&agr_id, Some(10.0))
                            .await;
                        log::info!("new agreement with: {}", issuer);
                        if let Ok(activity_id) =
                            activity_api_clone.control().create_activity(&agr_id).await
                        {
                            log::info!("activity created: {}", activity_id);
                            if let Ok(batch_id) = activity_api_clone
                                .control()
                                .exec(script, &activity_id)
                                .await
                            {
                                let mut all_res = vec![];
                                loop {
                                    log::info!("getting state of running activity {}", activity_id);
                                    if let Ok(state) =
                                        activity_api_clone.state().get_state(&activity_id).await
                                    {
                                        if !state.alive() {
                                            break;
                                        }
                                        if let Ok(res) = activity_api_clone
                                            .control()
                                            .get_exec_batch_results(
                                                &activity_id,
                                                &batch_id,
                                                None,
                                                None,
                                            )
                                            .await
                                        {
                                            log::debug!("batch_results: {}", res.len());
                                            all_res = res;
                                        }
                                        if all_res.len() >= num_cmds {
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                    tokio::time::delay_until(
                                        tokio::time::Instant::now() + Duration::from_secs(3),
                                    )
                                    .await;
                                }
                                log::info!("activity finished: {}", activity_id);
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
                                    .filter_map(|(i, r)| match run_ind.contains(&i) {
                                        // stdout: {}\nstdout;
                                        true => Some(r.message.unwrap_or("".to_string()))
                                            .map(only_stdout),
                                        false => None,
                                    })
                                    .collect();
                                let _ = payment_manager_clone
                                    .send(payment_manager::AcceptAgreement {
                                        agreement_id: agr_id.clone(),
                                    })
                                    .await;
                                // TODO not sure if this is not called too early
                                let _ = activity_api_clone
                                    .control()
                                    .destroy_activity(&activity_id)
                                    .await;
                                let _ = output_tx_clone.send((i, output)).await;
                            } else {
                                log::error!("exec failed!");
                            }
                        }
                    });
                }
                proposals = vec![];
                let mut outputs = vec!["".to_string(); providers_num];
                output_rx
                    .take(providers_num)
                    .for_each(|(prov_id, output)| {
                        outputs[prov_id] = output;
                        futures::future::ready(())
                    })
                    .await;
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
            /*if time_start.elapsed() > timeout {
                log::warn!("timeout")
            }*/
            tokio::time::delay_until(tokio::time::Instant::now() + Duration::from_secs(3)).await;
        }
        log::info!("all tasks completed and paid for.");

        Ok(())
    }

    async fn create_demand(&self) -> Demand {
        // "golem.node.debug.subnet" == "mysubnet", TODO
        let (digest, url) = self.task_package.publish().await.unwrap();
        let url_with_hash = format!("hash:sha3:{}:{}", digest, url);

        log::debug!("srv.comp.wasm.task_package: {}", url_with_hash);

        Demand::new(
            serde_json::json!({
                "golem": {
                    "node.id.name": self.name,
                    "srv.comp.wasm.task_package": url_with_hash,
                    "srv.comp.expiration":
                        (chrono::Utc::now() + chrono::Duration::minutes(10)).timestamp_millis(), // TODO
                },
            }),
            self.constraints.to_string(),
        )
    }
}

/*
struct GetStatus;

impl Message for GetStatus {
    type Result = f32;
}

impl Handler<GetStatus> for Requestor {
    type Result = f32;

    fn handle(&mut self, _msg: GetStatus, _ctx: &mut Self::Context) -> Self::Result {
        1.0 // TODO
    }
}

pub async fn requestor_monitor(task_session: Addr<Requestor>) -> Result<(), ()> {
    /* TODO attach to the actor */
    let progress_bar = ProgressBar::new(100);
    progress_bar.set_style(
        ProgressStyle::default_bar()
            .progress_chars("=> ")
            .template("{elapsed_precise} [{bar:40}] {msg}"),
    );
    //progress_bar.set_message("Running tasks");
    for _ in 0..100000 {
        //progress_bar.inc(1);
        let status = task_session.send(GetStatus).await;
        //log::error!("Here {:?}", status);
        tokio::time::delay_for(Duration::from_millis(950)).await;
    }
    //progress_bar.finish();
    Ok(())
}
*/
