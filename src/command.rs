use anyhow::{anyhow, Result};
use std::{
    collections::{HashMap, HashSet},
    iter::FromIterator,
};
use url::Url;
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
    Transfer { from: String, to: String },
    /// Path to file(s) to upload.
    Upload(String),
    /// Path to file(s) to download.
    Download(String),
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
///     upload("input.txt".to_string());
///     run("main", "/workdir/input.txt".to_string(), "/workdir/output.txt".to_string());
///     download("output.txt".to_string())
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
    pub(super) fn get_uploads(&self) -> Vec<String> {
        self.0
            .iter()
            .filter_map(|cmd| match cmd {
                Command::Upload(str) => Some(str.clone()),
                _ => None,
            })
            .collect()
    }

    pub(super) fn get_downloads(&self) -> Vec<String> {
        self.0
            .iter()
            .filter_map(|cmd| match cmd {
                Command::Download(str) => Some(str.clone()),
                _ => None,
            })
            .collect()
    }

    pub(super) fn as_exe_script_and_info(
        &self,
        upload_urls: &HashMap<String, Url>,
        download_urls: &HashMap<String, Url>,
    ) -> Result<(ExeScriptRequest, usize, HashSet<usize>)> {
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
                    json!({"run": obj})
                }
                Command::Transfer { from, to } => json!({"transfer": { "from": from, "to": to }}),
                Command::Upload(path) => serde_json::json!({ "transfer": {
                    "from": upload_urls[path],
                    "to": format!("container:/{}", path),
                }}),
                Command::Download(path) => serde_json::json!({ "transfer": {
                    "from": format!("container:/{}", path),
                    "to": download_urls[path],
                }}),
            })
        }

        Ok((
            ExeScriptRequest::new(serde_json::to_string_pretty(&res)?),
            res.len(),
            run_ind,
        ))
    }
}
