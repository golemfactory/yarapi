use anyhow::Result;
use futures::future::{select, FutureExt};
use std::collections::HashMap;
use std::path::PathBuf;
use structopt::StructOpt;

use ya_agreement_utils::{constraints, ConstraintKey, Constraints};
use ya_requestor_sdk::{commands, CommandList, Image::GVMKit, Package, Requestor};

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
async fn main() -> Result<()> {
    let _ = dotenv::dotenv()?;
    env_logger::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::from_args();
    let package = args.package.clone().into();

    let requestor = Requestor::new("My Requestor", GVMKit, package)
        .with_max_budget_gnt(5)
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
