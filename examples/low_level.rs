use chrono::Utc;
use futures::prelude::*;
use std::ops::Add;
use structopt::StructOpt;
use ya_client::web::WebClient;
use yarapi::rest;
use yarapi::rest::{Activity, RunningBatch};

const PACKAGE : &str = "hash:sha3:61c73e07e72ac7577857181043e838d7c40b787e2971ceca6ccb5922:http://yacn.dev.golem.network.:8000/trusted-voting-mgr-787e2971ceca6ccb5922.ywasi";

async fn create_agreement(
    client: rest::WebClient,
    subnet: &str,
    runtime: &str,
) -> anyhow::Result<rest::Agreement> {
    let market = rest::Market::new(client)?;
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

    eprintln!("constraints={}", constraints);

    let proposals = subscrption.proposals();
    futures::pin_mut!(proposals);
    while let Some(proposal) = proposals.try_next().await? {
        eprintln!(
            "got proposal: {} -- from: {}, draft: {:?}",
            proposal.id(),
            proposal.issuer_id(),
            proposal.state()
        );
        if proposal.is_response() {
            let agreement = proposal.create_agreement(deadline).await?;
            eprintln!("created agreement {}", agreement.id());
            if let Err(e) = agreement.confirm().await {
                log::error!("wait_for_approval failed: {:?}", e);
                continue;
            }
            return Ok(agreement);
        }
        let id = proposal.counter_proposal(&props, &constraints).await?;
        eprintln!("got: {}", id);
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
    eprintln!("start batch: {}", batch.id());
    if let Err(e) = batch
        .events()
        .try_for_each(|event| {
            eprintln!("event: {:?}", event);
            future::ok(())
        })
        .await
    {
        log::error!("batch error: {:?}", e)
    }
    eprintln!("credentials={:?}", activity.credentials());
    activity.destroy().await?;
    Ok(())
}

#[actix_rt::main]
pub async fn main() -> anyhow::Result<()> {
    let args = Args::from_args();

    env_logger::init();
    let client = WebClient::with_token(&args.appkey);
    let session = rest::Session::with_client(client.clone());

    let subnet = args.subnet.as_ref().map(AsRef::as_ref).unwrap_or("sgx");

    let agreement =
        create_agreement(client, subnet, if args.secure { "sgx" } else { "wasmtime" }).await?;
    if args.secure {
        do_init(session.create_secure_activity(&agreement).await?).await?;
    } else {
        do_init(session.create_activity(&agreement).await?).await?;
    }
    Ok(())
}
