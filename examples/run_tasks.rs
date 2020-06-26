use ya_agreement_utils::{constraints, ConstraintKey, Constraints};
use ya_requestor_sdk::{
    commands, requestor_monitor, CommandList, Image::WebAssembly, Location::File, Requestor,
};

#[actix_rt::main]
async fn main() -> Result<(), ()> {
    let _ = dotenv::dotenv().ok();
    env_logger::init();

    let requestor_actor = Requestor::new(
        "My Requestor",
        WebAssembly((1, 0, 0).into()),
        File("test-wasm.zip".into()),
    )
    .with_max_budget_gnt(5)
    .with_constraints(constraints![
        "golem.inf.mem.gib" > 0.5,
        "golem.inf.storage.gib" > 1.0
    ])
    .with_tasks(vec!["1", "2", "3"].into_iter().map(|i| {
        commands! {
            deploy;
            start;
            //copy("input.txt");
            transfer("TODO GFTP URL", "container:/input.txt");
            run("test-wasm", i);
            transfer("container:/output.txt", "TODO GFTP UPLOAD URL");
        }
    }))
    .run();

    requestor_monitor(requestor_actor).await?;
    Ok(())
}
