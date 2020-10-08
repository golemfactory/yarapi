use chrono::Utc;
use futures::prelude::*;
use std::ops::Add;
use structopt::StructOpt;
use ya_client::web::WebClient;
use yarapi::rest::{self, RunningBatch as _};

const PACKAGE : &str = "hash:sha3:61c73e07e72ac7577857181043e838d7c40b787e2971ceca6ccb5922:http://yacn.dev.golem.network.:8000/trusted-voting-mgr-787e2971ceca6ccb5922.ywasi";

async fn create_agreement(
    market: rest::Market,
    subnet: &str,
    runtime: &str,
) -> anyhow::Result<rest::Agreement> {
    let deadline = Utc::now().add(chrono::Duration::minutes(15));
    let ts = deadline.timestamp_millis();
    let props = serde_json::json!({
        "golem.node.id.name": "operator",
        "golem.node.debug.subnet": subnet,
        "golem.srv.comp.task_package": PACKAGE,
        "golem.srv.comp.expiration": ts
    });
    let constraints = format!(
        "(&(golem.runtime.name={runtime})(golem.node.debug.subnet={subnet}))",
        runtime = runtime,
        subnet = subnet
    );
    let subscrption = market.subscribe(&props, &constraints).await?;

    log::info!("constraints={}", constraints);

    let proposals = subscrption.proposals();
    futures::pin_mut!(proposals);
    while let Some(proposal) = proposals.try_next().await? {
        log::info!(
            "got proposal: {} -- from: {}, draft: {:?}",
            proposal.id(),
            proposal.issuer_id(),
            proposal.state()
        );
        if proposal.is_response() {
            let agreement = proposal.create_agreement(deadline).await?;
            log::info!("created agreement {}", agreement.id());
            if let Err(e) = agreement.confirm().await {
                log::error!("wait_for_approval failed: {:?}", e);
                continue;
            }
            return Ok(agreement);
        }
        let id = proposal.counter_proposal(&props, &constraints).await?;
        log::info!("got: {}", id);
    }
    unimplemented!()
}

#[derive(StructOpt)]
struct Args {
    #[structopt(long)]
    subnet: Option<String>,
    #[structopt(long)]
    secure: bool,
    #[structopt(long, env = "YAGNA_APPKEY")]
    appkey: String,
}

async fn do_init(activity: impl rest::Activity) -> anyhow::Result<()> {
    if let Some(c) = activity.credentials() {
        log::info!("credentials= {}\n\n", serde_json::to_string_pretty(&c)?);
    }

    let batch = activity
        .exec(vec![
            rest::ExeScriptCommand::Deploy {},
            rest::ExeScriptCommand::Start { args: vec![] },
            rest::ExeScriptCommand::Run {
                entry_point: "trusted-voting-mgr".to_string(),
                args: vec![
                    "init".to_string(),
                    "aea5db67524e02a263b9339fe6667d6b577f3d4c".to_string(),
                    "1".to_string(),
                ],
            },
        ])
        .await?;
    log::info!("start batch: {}", batch.id());
    if let Err(e) = batch
        .events()
        .try_for_each(|event| {
            log::info!("event: {:?}", event);
            future::ok(())
        })
        .await
    {
        log::error!("batch error: {:?}", e)
    }

    activity.destroy().await?;
    Ok(())
}

#[actix_rt::main]
pub async fn main() -> anyhow::Result<()> {
    let args = Args::from_args();
    std::env::set_var("RUST_LOG", "info,yarapi::drop=debug");
    env_logger::init();
    let client = WebClient::with_token(&args.appkey);
    let session = rest::Session::with_client(client.clone());

    session
        .with(async {
            let subnet = args.subnet.as_ref().map(AsRef::as_ref).unwrap_or("sgx");

            let agreement = create_agreement(
                session.market()?,
                subnet,
                if args.secure { "sgx" } else { "wasmtime" },
            )
            .await?;
            if args.secure {
                do_init(session.create_secure_activity(&agreement).await?).await?;
            } else {
                do_init(session.create_activity(&agreement).await?).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .unwrap_or_else(|| anyhow::bail!("ctrl-c caught"))
}
