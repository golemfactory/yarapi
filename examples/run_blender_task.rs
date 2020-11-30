use std::path::PathBuf;
use std::time::Duration;
use structopt::StructOpt;

use ya_client::model::activity::{CommandOutput, RuntimeEventKind};
use yarapi::{
    agreement::{constraints, ConstraintKey, Constraints},
    commands,
    requestor::{Image, Package, Requestor},
};

#[derive(StructOpt)]
struct Args {
    /// Only include providers belonging to this sub-network
    #[structopt(long)]
    subnet: String,
    #[structopt(long)]
    scene: PathBuf,
}

#[actix_rt::main]
async fn main() -> anyhow::Result<()> {
    dotenv::dotenv().ok();
    env_logger::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Args = Args::from_args();
    let tmp_dir = tempdir::TempDir::new("example")?;
    let tmp_path = tmp_dir.path().to_owned();

    let package = Package::Url {
        digest: String::from("9a3b5d67b0b27746283cb5f287c13eab1beaa12d92a9f536b747c7ae"),
        url: String::from("http://3.249.139.167:8000/local-image-c76719083b.gvmi"),
    };

    Requestor::new("My Requestor", Image::GVMKit((0, 1, 0).into()), package)
        .with_subnet(args.subnet.clone())
        .with_max_budget_ngnt(10)
        .with_timeout(Duration::from_secs(12 * 60))
        .with_constraints(constraints![
            "golem.inf.mem.gib" > 0.5,
            "golem.inf.storage.gib" > 1.0
        ])
        .with_tasks(vec![1, 2].into_iter().map(|i| {
            let crop = serde_json::json!({
                "outfilebasename": "out",
                "borders_x": [0.0, 1.0],
                "borders_y": [0.0, 1.0]
            });
            let json = serde_json::json!({
                "scene_file": "/golem/resource/scene.blend",
                "resolution": (400, 300),
                "use_compositing": false,
                "crops": [crop],
                "samples": 100,
                "frames": [i],
                "output_format": "PNG",
                "RESOURCES_DIR": "/golem/resources",
                "WORK_DIR": "/golem/work",
                "OUTPUT_DIR": "/golem/output",
            });

            let params_file = tmp_path.join(format!("params_{}.json", i));
            let _ = std::fs::write(&params_file, json.to_string());

            commands! {
                upload(args.scene.clone(), "/golem/resource/scene.blend");
                upload(params_file, "/golem/work/params.json");
                run("/golem/entrypoints/run-blender.sh");
                download(format!("/golem/output/out{:04}.png", i), format!("out{:04}.png", i))
            }
        }))
        .on_event(|e| match e.kind {
            RuntimeEventKind::Started { command } => println!("> started {:?}", command),
            RuntimeEventKind::Finished {
                return_code,
                message,
            } => println!("> finished with {}: {:?}", return_code, message),
            RuntimeEventKind::StdOut(out) => println!("{}", output_to_string(out)),
            RuntimeEventKind::StdErr(out) => eprintln!("{}", output_to_string(out)),
        })
        .on_completed(|activity_id, output| {
            println!("{} => {:?}", activity_id, output);
        })
        .run()
        .await
}

fn output_to_string(out: CommandOutput) -> String {
    match out {
        CommandOutput::Str(s) => s.trim().to_string(),
        CommandOutput::Bin(v) => {
            let s = String::from_utf8_lossy(&v.as_slice());
            s.trim().to_string()
        }
    }
}
