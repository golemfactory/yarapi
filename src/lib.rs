use actix::prelude::*;
use bigdecimal::BigDecimal;
//use indicatif::{ProgressBar, ProgressStyle};
use futures::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use url::Url;
use ya_agreement_utils::{constraints, ConstraintKey, Constraints};
use ya_client::activity::ActivityRequestorApi;
use ya_client::model::activity::ExeScriptRequest;
use ya_client::{
    market::MarketRequestorApi,
    model::{
        self,
        market::{proposal::State, AgreementProposal, Demand, RequestorEvent},
    },
    payment::PaymentRequestorApi,
};

/* TODO don't use PaymentManager from gwasm-runner */
#[allow(dead_code)]
#[allow(unused_variables)]
#[allow(unused_must_use)]
mod payment_manager;

#[derive(Clone)]
pub enum Location {
    File(String),
    URL(String),
}

#[derive(Clone)]
pub enum Image {
    WebAssembly(semver::Version),
    GVMKit,
}

#[derive(Clone)]
pub enum Command {
    Deploy,
    Start,
    Run(Vec<String>),
    Transfer { from: String, to: String },
    Upload(String),
    Download(String),
}

#[derive(Clone)]
pub struct CommandList(Vec<Command>);

impl CommandList {
    pub fn new(v: Vec<Command>) -> Self {
        Self(v)
    }
    pub fn get_upload_files(&self) -> Vec<String> {
        self.0
            .iter()
            .filter_map(|cmd| match cmd {
                Command::Upload(str) => Some(str.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn get_download_files(&self) -> Vec<String> {
        self.0
            .iter()
            .filter_map(|cmd| match cmd {
                Command::Download(str) => Some(str.clone()),
                _ => None,
            })
            .collect()
    }
}

impl CommandList {
    pub fn to_exe_script_and_info(
        &self,
        gftp_upload_urls: &HashMap<String, Url>,
        gftp_download_urls: &HashMap<String, Url>,
    ) -> Result<(ExeScriptRequest, usize, HashSet<usize>), anyhow::Error> {
        let mut res = vec![];
        let mut run_ind = HashSet::new();
        for (i, cmd) in vec![Command::Deploy, Command::Start]
            .iter()
            .chain(self.0.iter())
            .enumerate()
        {
            res.push(match cmd {
                Command::Deploy => serde_json::json!({ "deploy": {} }),
                Command::Start => serde_json::json!({ "start": { "args": [] }}),
                Command::Run(vec) => {
                    run_ind.insert(i);
                    serde_json::json!({ "run": { // TODO depends on ExeUnit type
                        "entry_point": vec[0],
                        "args": &vec[1..]
                    }})
                }
                Command::Transfer { from, to } => serde_json::json!({ "transfer": {
                    "from": from,
                    "to": to,
                }}),
                // TODO!!! container paths should be configurable (not only /workdir/)
                Command::Upload(path) => serde_json::json!({ "transfer": {
                    "from": gftp_upload_urls[path],
                    "to": format!("container:/workdir/{}", path),
                }}),
                Command::Download(path) => serde_json::json!({ "transfer": {
                    "from": format!("container:/workdir/{}", path),
                    "to": gftp_download_urls[path],
                }}),
            })
        }
        Ok((
            ExeScriptRequest::new(serde_json::to_string_pretty(&res)?),
            res.len(),
            run_ind,
        ))
    }
}

#[derive(Clone)]
pub struct Requestor {
    name: String,
    image_type: Image,
    location: Location,
    constraints: Constraints,
    tasks: Vec<CommandList>,
    timeout: Duration,
    budget: BigDecimal,
    status: String,
    on_completed: Option<Arc<dyn Fn(Vec<String>)>>,
}

impl Requestor {
    pub fn new<T: Into<String>>(name: T, image_type: Image, location: Location) -> Self {
        Self {
            name: name.into(),
            image_type,
            location,
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
    pub fn run(self) -> Addr<Requestor> {
        self.start()
    }
    fn create_demand(&self, image_url: &Url) -> Demand {
        // let hex = format!("{:x}", <sha3::Sha3_224 as Digest>::digest(image.as_slice()));
        // "golem.node.debug.subnet" == "mysubnet", TODO
        Demand::new(
            serde_json::json!({
                "golem": {
                    "node.id.name": self.name,
                    "srv.comp.wasm.task_package": format!("hash:sha3:674eeaed2c83c6a71480016154547d548d20d68371c11f0abfc0eb9d:{}", image_url), /* TODO!!! */
                    "srv.comp.expiration":
                        (chrono::Utc::now() + chrono::Duration::minutes(10)).timestamp_millis(), // TODO
                },
            }),
            self.constraints.to_string(),
        )
    }
    async fn create_gftp_urls(
        &self,
    ) -> Result<(HashMap<String, Url>, HashMap<String, Url>), anyhow::Error> {
        let mut upload_urls = HashMap::new();
        let mut download_urls = HashMap::new();
        log::info!("serving files using gftp");
        for task in &self.tasks {
            for name in task.get_upload_files() {
                match Path::new(&name).canonicalize() {
                    Ok(path) => {
                        log::info!("gftp requestor->provider {:?}", path);
                        let url = gftp::publish(&path).await?;
                        log::debug!("upload to provider: {}", url);
                        upload_urls.insert(name, url);
                    }
                    Err(e) => log::error!("file: {} error: {}", name, e),
                }
            }
            for name in task.get_download_files() {
                log::info!("gftp provider->requestor {}", name);
                let path = Path::new(&name);
                let url = gftp::open_for_upload(&path).await?;
                log::info!("download from provider: {}", url);
                download_urls.insert(name, url);
            }
        }
        Ok((upload_urls, download_urls))
    }
}

#[macro_export]
macro_rules! expand_cmd {
    (deploy) => { $crate::Command::Deploy };
    (start) => { $crate::Command::Start };
    (stop) => { $crate::Command::Stop };
    (run ( $($e:expr),* )) => {{
        $crate::Command::Run(vec![ $($e.to_string()),* ])
    }};
    (transfer ( $e:expr, $f:expr)) => {
        $crate::Command::Transfer { from: $e.to_string(), to: $f.to_string() }
    };
    (upload ( $e:expr )) => {
        $crate::Command::Upload( $e.to_string() )
    };
    (download ( $e:expr )) => {
        $crate::Command::Download( $e.to_string() )
    };
}

#[macro_export]
macro_rules! commands_helper {
    () => {};
    ( $i:ident ( $($param:expr),* ) $(;)* ) => {{
        vec![$crate::expand_cmd!($i ( $($param),* ))]
    }};
    ( $i:tt $(;)* ) => {{
        vec![$crate::expand_cmd!($i)]
    }};
    ( $i:ident ( $($param:expr),* ) ; $( $t:tt )* ) => {{
        let mut tail = $crate::commands_helper!( $($t)* );
        tail.push($crate::expand_cmd!($i ( $($param),* )));
        tail
    }};
    ( $i:tt ; $( $t:tt )* ) => {{
        let mut tail = $crate::commands_helper!( $($t)* );
        tail.push($crate::expand_cmd!($i));
        tail
    }};
}

#[macro_export]
macro_rules! commands {
    ( $( $t:tt )* ) => {{
        let mut v = $crate::commands_helper!( $($t)* );
        v.reverse();
        CommandList::new(v)
    }};
}

impl Actor for Requestor {
    type Context = Context<Self>;

    /* TODO cleanup:
    release_allocation(); // in payment_manager?
    unsubscribe();
    */

    fn started(&mut self, ctx: &mut Self::Context) {
        let app_key = std::env::var("YAGNA_APPKEY").unwrap();
        //let client = ya_client::web::WebClient::with_token(&app_key).unwrap();
        let client = ya_client::web::WebClient::builder()
            .auth_token(&app_key)
            .build();
        let market_api: MarketRequestorApi = client.interface().unwrap();
        //let activity_api: ActivityRequestorControlApi = client.interface().unwrap();
        let activity_api: ActivityRequestorApi = client.interface().unwrap();
        let payment_api: PaymentRequestorApi = client.interface().unwrap();
        let self_copy = self.clone();
        //let timeout = self.timeout;
        let providers_num = self.tasks.len();

        ctx.spawn(
            async move {
                /* publish image file */
                let url_to_image_file = match &self_copy.location {
                    Location::File(name) => {
                        let image_path = Path::new(name).canonicalize().unwrap();
                        log::info!("publishing image file {}", image_path.display());
                        gftp::publish(&image_path).await?
                    }
                    Location::URL(url) => Url::parse(&url)?,
                };
                log::info!("published image as {}", url_to_image_file);
                let (gftp_upload_urls, gftp_download_urls) = self_copy.create_gftp_urls().await?;
                let demand = self_copy.create_demand(&url_to_image_file);
                //log::info!("Demand: {}", serde_json::to_string(&demand).unwrap());

                let subscription_id = market_api.subscribe(&demand).await?;
                log::info!("subscribed to Market API ( id : {} )", subscription_id);

                let allocation = payment_api
                    .create_allocation(&model::payment::NewAllocation {
                        total_amount: self_copy.budget,
                        timeout: None,
                        make_deposit: false,
                    })
                    .await?;
                log::info!("allocated {} GNT.", &allocation.total_amount);

                /* TODO accept invoice after computations */
                let _payment_manager =
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
                                    let bespoke_proposal =
                                        match proposal.counter_demand(demand.clone()) {
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
                                            .counter_proposal(
                                                &bespoke_proposal,
                                                &subscription_id_clone,
                                            )
                                            .await;
                                    });
                                } else {
                                    proposals.push(proposal.clone());
                                    log::debug!(
                                        "got {} answer(s) to counter proposal",
                                        proposals.len()
                                    );
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
                        let (output_tx, output_rx) =
                            futures::channel::mpsc::unbounded::<(usize, String)>();
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
                            let (script, num_cmds, run_ind) = self_copy.tasks[i]
                                .to_exe_script_and_info(&gftp_upload_urls, &gftp_download_urls)?;
                            log::info!("exe script: {:?}", script);
                            let mut output_tx_clone = output_tx.clone();
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
                                            log::info!(
                                                "getting state of running activity {}",
                                                activity_id
                                            );
                                            if let Ok(state) = activity_api_clone
                                                .state()
                                                .get_state(&activity_id)
                                                .await
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
                                                tokio::time::Instant::now()
                                                    + Duration::from_secs(3),
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
                                        let _ = output_tx_clone.send((i, output)).await;
                                    // TODO not sure if this should be here: activity_api_clone
                                    // .control().destroy_activity(&activity_id).await; */
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
                        if let Some(fun) = self_copy.on_completed.clone() {
                            fun(outputs);
                            state = ComputationState::Done;
                        }
                        //let events = market_api.unsubscribe(&subscription_id).await;
                    }
                    /*if time_start.elapsed() > timeout {
                        log::warn!("timeout")
                    }*/
                    tokio::time::delay_until(tokio::time::Instant::now() + Duration::from_secs(3))
                        .await;
                }
                Ok::<_, anyhow::Error>(())
            }
            .into_actor(self) /* TODO send AcceptAgreement */
            .then(|_result, _ctx, _| fut::ready(())),
        );
    }
}

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

/*
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
