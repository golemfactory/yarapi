use std::path::PathBuf;
use std::time::Duration;
use structopt::StructOpt;

use yarapi::{
    agreement::{constraints, ConstraintKey, Constraints},
    commands,
    requestor::{Image, Package, Requestor},
};

#[derive(StructOpt)]
struct Args {
    #[structopt(flatten)]
    package: Location,
    input: PathBuf,
}

#[derive(Debug, Clone, StructOpt)]
enum Location {
    Local { path: PathBuf },
    Url { url: String, digest: String },
}

impl From<Location> for Package {
    fn from(args: Location) -> Self {
        match args {
            Location::Local { path } => Package::Archive(path),
            Location::Url { digest, url } => Package::Url { digest, url },
        }
    }
}

#[actix_rt::main]
async fn main() -> anyhow::Result<()> {
    dotenv::dotenv().ok();
    env_logger::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::from_args();
    let package = args.package.clone().into();

    Requestor::new("My Requestor", Image::GVMKit((0, 2, 4).into()), package)
        .with_subnet("community.4")
        .with_max_budget_glm(5)
        .with_timeout(Duration::from_secs(12 * 60))
        .with_constraints(constraints![
            "golem.inf.mem.gib" > 0.5,
            "golem.inf.storage.gib" > 1.0
        ])
        .with_tasks(vec!["1"].into_iter().map(move |i| {
            commands! {
                upload(args.input.clone(), "/workdir/input");
                run("/bin/ls", "-la", "/workdir/input");
                run("/bin/cp", "/workdir/input", "/workdir/output");
                download("/workdir/output", format!("output-{}", i))
            }
        }))
        .on_completed(|activity_id, output| {
            println!("{} => {:?}", activity_id, output);
        })
        .run()
        .await
}
