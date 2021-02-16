#![allow(unused)]
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
    /// Only include providers belonging to this sub-network
    #[structopt(long, default_value = "testnet")]
    subnet: String,
    /// Request secure computations (SGX)
    #[structopt(long)]
    secure: bool,
    /// Providers will download task input from this URL
    #[structopt(short, long)]
    upload_url: Option<String>,
    /// Providers will upload task output to this URL
    #[structopt(short, long)]
    download_url: Option<String>,
    /// Requestor name
    #[structopt(short, long, default_value = "My Requestor")]
    name: String,
    #[structopt(flatten)]
    package: Location,
}

#[derive(Debug, Clone, StructOpt)]
enum Location {
    /// Task package from disk
    File { path: PathBuf },
    /// Task package from URL
    Url {
        #[structopt(short, long)]
        url: String,
        #[structopt(short, long)]
        digest: String,
    },
}

impl From<Location> for Package {
    fn from(args: Location) -> Self {
        match args {
            Location::File { path } => Package::Archive(path),
            Location::Url { digest, url } => Package::Url { digest, url },
        }
    }
}

#[actix_rt::main]
async fn main() -> anyhow::Result<()> {
    dotenv::dotenv().ok();
    env_logger::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Args = Args::from_args();
    let package: Package = args.package.clone().into();

    if args.secure {
        Requestor::new(&args.name, Image::Sgx((0, 1, 0).into()), package).secure()
    } else {
        Requestor::new(&args.name, Image::Wasm((0, 1, 0).into()), package)
    }
    .with_subnet(&args.subnet)
    .with_max_budget_glm(10)
    .with_timeout(Duration::from_secs(12 * 60))
    .with_constraints(constraints![
        "golem.inf.mem.gib" > 0.4,
        "golem.inf.storage.gib" > 0.1
    ])
    .with_tasks(vec!["1", "2"].into_iter().map(|i| {
        if args.secure {
            // TODO: define a proper set of commands when the SGX runtime is ready
            commands![
                run("main", "/input/input.txt", "/output/output.txt");
            ]
        } else {
            commands![
                upload(format!("input-{}.txt", i), "/input/input.txt");
                run("main", "/input/input.txt", "/output/output.txt");
                download("/output/output.txt", format!("output-{}.txt", i))
            ]
        }
    }))
    .on_completed(|activity_id, output| {
        println!("{} => {:?}", activity_id, output);
    })
    .run()
    .await
}
