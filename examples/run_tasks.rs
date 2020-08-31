use anyhow::Result;
use futures::future::{select, FutureExt};
use std::collections::HashMap;

use ya_agreement_utils::{constraints, ConstraintKey, Constraints};
use ya_requestor_sdk::{commands, CommandList, Image::WebAssembly, Package::Archive, Requestor};

#[actix_rt::main]
async fn main() -> Result<()> {
    let _ = dotenv::dotenv()?;
    env_logger::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let requestor = Requestor::new(
        "My Requestor",
        WebAssembly((1, 0, 0).into()),
        Archive("test-wasm.zip".into()),
    )
    .with_max_budget_gnt(5)
    .with_constraints(constraints![
        "golem.inf.mem.gib" > 0.5,
        "golem.inf.storage.gib" > 1.0
    ])
    .with_tasks(vec!["1", "2"].into_iter().map(|i| {
        commands! {
            upload(format!("input-{}.txt", i), "/workdir/input.txt");
            run("main", i, "/workdir/input.txt", "/workdir/output.txt");
            download("/workdir/output.txt", format!("output-{}.txt", i))
        }
    }))
    .on_completed(|outputs: HashMap<String, String>| {
        for (prov_id, output) in outputs {
            println!("{} => {}", prov_id, output);
        }
    })
    .run();

    let ctrl_c = actix_rt::signal::ctrl_c().then(|r| async move {
        match r {
            Ok(_) => Ok(log::warn!("interrupted: ctrl-c detected!")),
            Err(e) => Err(anyhow::Error::from(e)),
        }
    });

    select(requestor.boxed_local(), ctrl_c.boxed_local())
        .await
        .into_inner()
        .0
}
