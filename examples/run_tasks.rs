use anyhow::{anyhow, Result};
use futures::{future::FutureExt, pin_mut, select};
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
    .on_completed(|outputs: Vec<String>| {
        outputs
            .iter()
            .enumerate()
            .for_each(|(i, o)| println!("task #{}: {}", i, o));
    })
    .run()
    .fuse();
    let ctrl_c = actix_rt::signal::ctrl_c().fuse();

    pin_mut!(requestor, ctrl_c);

    select! {
        comp_res = requestor => comp_res,
        _ = ctrl_c => Err(anyhow!("interrupted: ctrl-c detected!")),
    }
}
