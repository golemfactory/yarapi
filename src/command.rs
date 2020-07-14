use anyhow::{anyhow, Result};
use std::{
    collections::HashSet,
    iter::FromIterator,
    path::{Path, PathBuf},
};
use ya_client::model::activity::ExeScriptRequest;

/// Represents supported exe-script commands.
///
/// Note that when specifying the `CommandList`, specifying
/// `Deploy` and `Start` explicitly is optional; therefore,
/// skipping those two is fine.
#[derive(Clone)]
pub enum Command {
    /// Deploy the container.
    Deploy,
    /// Start the container.
    Start, // TODO add args
    Run(Vec<String>),
    /// Transfer from `from` url to `to` url.
    ///
    /// TODO explain which urls are valid: [`file:/`, `http://`, `gftp:/`, `container:/`].
    Transfer {
        from: String,
        to: String,
    },
    /// Path to file(s) to upload.
    Upload {
        from: PathBuf,
        to: String,
    },
    /// Path to file(s) to download.
    Download {
        from: String,
        to: PathBuf,
    },
}

/// Represents a list of commands to execute at the remote node.
/// This is equivalent to the exe-script you'd write out manually when
/// manually launching a Yagna task.
///
/// Note that when specifying the `CommandList`, specifying
/// `Deploy` and `Start` explicitly is optional; therefore,
/// skipping those two is fine.
///
/// ## Example:
/// ```rust
/// use ya_requestor_sdk::{commands, CommandList};
///
/// let script = commands![
///     upload("input.txt".to_string(), "/workdir/input.txt".to_string());
///     run("main", "/workdir/input.txt".to_string(), "/workdir/output.txt".to_string());
///     download("/workdir/output.txt".to_string(), "output.txt".to_string())
/// ];
/// ```
#[derive(Clone)]
pub struct CommandList(Vec<Command>);

impl CommandList {
    pub fn new(v: impl IntoIterator<Item = Command>) -> Self {
        // TODO validate the order of commands; i.e., deploy before start,
        // start before the rest, etc.
        Self(Vec::from_iter(v))
    }

    pub(super) async fn into_exe_script(self) -> Result<(ExeScriptRequest, usize, HashSet<usize>)> {
        use serde_json::{json, map::Map};

        let mut res = vec![];
        let mut run_ind = HashSet::new();
        // TODO verify the `CommandList` doesn't already contain `Command::Deploy` or
        // `Command::Start`.
        for (i, cmd) in vec![Command::Deploy, Command::Start]
            .iter()
            .chain(self.0.iter())
            .enumerate()
        {
            res.push(match cmd {
                Command::Deploy => json!({"deploy": {}}),
                Command::Start => json!({"start": {"args": []}}),
                Command::Run(vec) => {
                    // TODO "run" depends on ExeUnit type
                    run_ind.insert(i);
                    let mut obj = Map::new();
                    let entry_point = vec.get(0).ok_or(anyhow!(
                        "expected at least one entry in Command::Run: entry_point"
                    ))?;
                    obj.insert("entry_point".to_string(), json!(entry_point));
                    if let Some(args) = vec.get(1..) {
                        obj.insert("args".to_string(), json!(args));
                    }
                    json!({ "run": obj })
                }
                Command::Transfer { from, to } => json!({"transfer": { "from": from, "to": to }}),
                Command::Upload { from, to } => serde_json::json!({ "transfer": {
                    "from": Self::get_upload(&from).await?,
                    "to": format!("container:{}", to),
                }}),
                Command::Download { from, to } => serde_json::json!({ "transfer": {
                    "from": format!("container:{}", from),
                    "to": Self::get_download(&to).await?,
                }}),
            })
        }

        Ok((
            ExeScriptRequest::new(serde_json::to_string_pretty(&res)?),
            res.len(),
            run_ind,
        ))
    }

    async fn get_upload(path: &Path) -> Result<String> {
        let path = path.canonicalize()?;
        log::info!("gftp requestor->provider {}", path.display());

        let url = gftp::publish(&path).await?.to_string();
        log::info!("upload to provider: {}", url);

        Ok(url)
    }

    async fn get_download(path: &Path) -> Result<String> {
        log::info!("gftp provider->requestor {}", path.display());

        let url = gftp::open_for_upload(&path).await?.to_string();
        log::info!("download from provider: {}", url);

        Ok(url)
    }
}
