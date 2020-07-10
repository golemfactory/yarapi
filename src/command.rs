use anyhow::{anyhow, Result};
use std::{
    collections::{HashMap, HashSet},
    iter::FromIterator,
};
use url::Url;
use ya_client::model::activity::ExeScriptRequest;

#[derive(Clone)]
pub enum Command {
    Deploy,
    Start, // TODO add args
    Run(Vec<String>),
    Transfer { from: String, to: String },
    Upload(String),
    Download(String),
}

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
