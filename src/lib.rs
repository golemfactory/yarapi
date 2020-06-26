use actix::prelude::*;
use bigdecimal::BigDecimal;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use std::{
    convert::{TryFrom, TryInto},
    time::Duration,
};
use url::Url;
use ya_agreement_utils::{constraints, ConstraintKey, Constraints};
use ya_client::model::activity::ExeScriptRequest;
use ya_client::{
    activity::ActivityRequestorControlApi, market::MarketRequestorApi, model,
    model::market::Demand, payment::PaymentRequestorApi,
};

mod market_negotiator;
mod payment_manager;

#[derive(Clone)]
pub enum WasmRuntime {
    Wasi(i32), /* Wasi version */
}

#[derive(Clone)]
pub struct ImageSpec {
    runtime: WasmRuntime,
    /* TODO */
}

impl ImageSpec {
    pub fn from_github<T: Into<String>>(_github_repository: T) -> Self {
        Self {
            runtime: WasmRuntime::Wasi(1),
            /* TODO http URL? */
        }
        /* TODO connect and download image specification */
    }
    pub fn from_url<T: Into<String>>(url: T) -> Self {
        Self {
            runtime: WasmRuntime::Wasi(1),
            /* TODO: gftp URL? */
        }
    }
    pub fn runtime(self, runtime: WasmRuntime) -> Self {
        Self { runtime }
    }
}

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
}

#[derive(Clone)]
pub struct CommandList(Vec<Command>);

impl CommandList {
    pub fn new(v: Vec<Command>) -> Self {
        Self(v)
    }
}

impl TryFrom<CommandList> for ExeScriptRequest {
    type Error = anyhow::Error;
    fn try_from(cmd_list: CommandList) -> Result<Self, anyhow::Error> {
        let mut res = vec![];
        for cmd in cmd_list.0 {
            res.push(match cmd {
                Command::Deploy => serde_json::json!({ "deploy": {} }),
                Command::Start => serde_json::json!({ "start": { "args": [] }}),
                Command::Run(vec) => serde_json::json!({ "run": { // TODO depends on ExeUnit type
                    "entry_point": "main",
                    "args": vec
                }}),
                Command::Transfer { from, to } => serde_json::json!({ "transfer": {
                    "from": from,
                    "to": to,
                }}),
            })
        }
        Ok(ExeScriptRequest::new(serde_json::to_string_pretty(&res)?))
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
        }
    }
    pub fn with_constraints(self, constraints: Constraints) -> Self {
        Self {
            constraints,
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
        Self {
            tasks: tasks.collect(),
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
                    "srv.comp.wasm.task_package": format!("hash:sha3:0x1352137839e66fd48e59e09d03d1f7229fc3150081e98159ab2107c5:{}", image_url), /* TODO!!! */
                    "srv.comp.expiration":
                        (chrono::Utc::now() + chrono::Duration::minutes(2)).timestamp_millis(), // TODO
                },
            }),
            self.constraints.to_string(),
        )
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

    fn started(&mut self, ctx: &mut Self::Context) {
        let app_key = "69c892de22d745e489b044f8a4ae35de";
        //let client = ya_client::web::WebClient::with_token(&app_key).unwrap();
        let client = ya_client::web::WebClient::builder()
            .auth_token(&app_key)
            .build();
        /* TODO URLs from env */
        let market_api: MarketRequestorApi = client.interface().unwrap();
        let activity_api: ActivityRequestorControlApi = client.interface().unwrap();
        let payment_api: PaymentRequestorApi = client.interface().unwrap();
        let self_copy = self.clone();

        ctx.spawn(
            async move {
                /* publish image file TODO real file */
                let url_to_image_file = match &self_copy.location {
                    Location::File(name) => {
                        let image_path = Path::new("test-wasm.zip").canonicalize().unwrap();
                        log::debug!("Publishing image file {}", image_path.display());
                        gftp::publish(&image_path).await?
                    }
                    Location::URL(url) => Url::parse(&url)?,
                };
                log::debug!("Published image as {}", url_to_image_file);
                let demand = self_copy.create_demand(&url_to_image_file);
                //log::info!("Demand: {}", serde_json::to_string(&demand).unwrap());

                /* TODO move market_api and payment_api calls to actors */
                let subscription_id = market_api.subscribe(&demand).await?;
                log::info!("Subscribed to Market API ( id : {} )", subscription_id);

                let allocation = payment_api
                    .create_allocation(&model::payment::NewAllocation {
                        total_amount: self_copy.budget,
                        timeout: None,
                        make_deposit: false,
                    })
                    .await?;
                log::info!("Allocated {} GNT.", &allocation.total_amount);

                /* start actors */
                let agreement_producer = market_negotiator::AgreementProducer::new(
                    market_api.clone(),
                    subscription_id,
                    demand.clone(),
                )
                .start();
                let payment_manager =
                    payment_manager::PaymentManager::new(payment_api.clone(), allocation).start();
                loop {
                    log::info!("waiting for new agreement");
                    let agreement_id = agreement_producer
                        .send(market_negotiator::NewAgreement)
                        .await??;
                    log::info!("got new agreement");
                    let activity_id = activity_api.create_activity(&agreement_id).await?;
                    let script: ExeScriptRequest = self_copy.tasks[0].clone().try_into()?; /* TODO!!! */
                    log::debug!("Exe Script: {:?}", script);
                    let res = activity_api.exec(script, &activity_id).await?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .into_actor(self)
            .then(|result, ctx, _| fut::ready(())), //.then(|result, ctx, _| {}),
        );
    }
}

struct GetStatus;

impl Message for GetStatus {
    type Result = f32;
}

impl Handler<GetStatus> for Requestor {
    type Result = f32;

    fn handle(&mut self, msg: GetStatus, ctx: &mut Self::Context) -> Self::Result {
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
        log::error!("Here {:?}", status);
        tokio::time::delay_for(Duration::from_millis(950)).await;
    }
    //progress_bar.finish();
    Ok(())
}
