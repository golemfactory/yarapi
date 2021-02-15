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
    #[structopt(long, default_value = "README.md")]
    input_file: PathBuf,
    #[structopt(long, env, default_value = "community.4")]
    subnet: String,
    #[structopt(flatten)]
    package: Location,
}

#[derive(Debug, Clone, StructOpt)]
enum Location {
    /// local file path
    Local { path: PathBuf },
    /// remote url + sha
    Url { url: String, digest: String },
    /// use it as `YAGNA_APPKEY=$(yagna app-key list --json | jq -r .values[0][1]) cargo run --example run_vm_task default`
    Default,
}

impl From<Location> for Package {
    fn from(args: Location) -> Self {
        match args {
            Location::Local { path } => Package::Archive(path),
            Location::Url { digest, url } => Package::Url { digest, url },
            Location::Default => Package::Url {
                digest: "9a3b5d67b0b27746283cb5f287c13eab1beaa12d92a9f536b747c7ae".to_string(),
                url: "http://yacn.dev.golem.network:8000/local-image-c76719083b.gvmi".to_string(),
            },
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
        .with_subnet(&args.subnet)
        .with_max_budget_glm(5)
        .with_timeout(Duration::from_secs(12 * 60))
        .with_constraints(constraints![
            "golem.inf.mem.gib" > 0.5,
            "golem.inf.storage.gib" > 1.0
        ])
        .with_tasks(vec!["1"].into_iter().map(move |i| {
            commands! {
                upload(args.input_file.clone(), "/golem/work/input-file");
                run("/bin/ls", "-la", "/golem/work");
                run("/bin/cp", "/golem/work/input-file", "/golem/output");
                download("/golem/output/input-file", format!("output-{}", i))
            }
        }))
        .on_completed(|activity_id, output| {
            println!("{} => {:?}", activity_id, output);
        })
        .run()
        .await
}
