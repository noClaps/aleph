use agent_client_protocol as acp;
use anyhow::{Context as _, Result, bail};
use file_icons::FileIcons;
use prompt_store::{PromptId, UserPromptId};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    str::FromStr,
};
use ui::{App, IconName, SharedString};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum MentionUri {
    File {
        abs_path: PathBuf,
    },
    PastedImage,
    Directory {
        abs_path: PathBuf,
    },
    Symbol {
        abs_path: PathBuf,
        name: String,
        line_range: RangeInclusive<u32>,
    },
    Thread {
        id: acp::SessionId,
        name: String,
    },
    TextThread {
        path: PathBuf,
        name: String,
    },
    Rule {
        id: PromptId,
        name: String,
    },
    Selection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        abs_path: Option<PathBuf>,
        line_range: RangeInclusive<u32>,
    },
    Fetch {
        url: Url,
    },
}

impl MentionUri {
    pub fn parse(input: &str) -> Result<Self> {
        fn parse_line_range(fragment: &str) -> Result<RangeInclusive<u32>> {
            let range = fragment
                .strip_prefix("L")
                .context("Line range must start with \"L\"")?;
            let (start, end) = range
                .split_once(":")
                .context("Line range must use colon as separator")?;
            let range = start
                .parse::<u32>()
                .context("Parsing line range start")?
                .checked_sub(1)
                .context("Line numbers should be 1-based")?
                ..=end
                    .parse::<u32>()
                    .context("Parsing line range end")?
                    .checked_sub(1)
                    .context("Line numbers should be 1-based")?;
            Ok(range)
        }

        let url = url::Url::parse(input)?;
        let path = url.path();
        match url.scheme() {
            "file" => {
                let path = url.to_file_path().ok().context("Extracting file path")?;
                if let Some(fragment) = url.fragment() {
                    let line_range = parse_line_range(fragment)?;
                    if let Some(name) = single_query_param(&url, "symbol")? {
                        Ok(Self::Symbol {
                            name,
                            abs_path: path,
                            line_range,
                        })
                    } else {
                        Ok(Self::Selection {
                            abs_path: Some(path),
                            line_range,
                        })
                    }
                } else if input.ends_with("/") {
                    Ok(Self::Directory { abs_path: path })
                } else {
                    Ok(Self::File { abs_path: path })
                }
            }
            "zed" => {
                if let Some(thread_id) = path.strip_prefix("/agent/thread/") {
                    let name = single_query_param(&url, "name")?.context("Missing thread name")?;
                    Ok(Self::Thread {
                        id: acp::SessionId(thread_id.into()),
                        name,
                    })
                } else if let Some(path) = path.strip_prefix("/agent/text-thread/") {
                    let name = single_query_param(&url, "name")?.context("Missing thread name")?;
                    Ok(Self::TextThread {
                        path: path.into(),
                        name,
                    })
                } else if let Some(rule_id) = path.strip_prefix("/agent/rule/") {
                    let name = single_query_param(&url, "name")?.context("Missing rule name")?;
                    let rule_id = UserPromptId(rule_id.parse()?);
                    Ok(Self::Rule {
                        id: rule_id.into(),
                        name,
                    })
                } else if path.starts_with("/agent/pasted-image") {
                    Ok(Self::PastedImage)
                } else if path.starts_with("/agent/untitled-buffer") {
                    let fragment = url
                        .fragment()
                        .context("Missing fragment for untitled buffer selection")?;
                    let line_range = parse_line_range(fragment)?;
                    Ok(Self::Selection {
                        abs_path: None,
                        line_range,
                    })
                } else {
                    bail!("invalid zed url: {:?}", input);
                }
            }
            "http" | "https" => Ok(MentionUri::Fetch { url }),
            other => bail!("unrecognized scheme {:?}", other),
        }
    }

    pub fn name(&self) -> String {
        match self {
            MentionUri::File { abs_path, .. } | MentionUri::Directory { abs_path, .. } => abs_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            MentionUri::PastedImage => "Image".to_string(),
            MentionUri::Symbol { name, .. } => name.clone(),
            MentionUri::Thread { name, .. } => name.clone(),
            MentionUri::TextThread { name, .. } => name.clone(),
            MentionUri::Rule { name, .. } => name.clone(),
            MentionUri::Selection {
                abs_path: path,
                line_range,
                ..
            } => selection_name(path.as_deref(), line_range),
            MentionUri::Fetch { url } => url.to_string(),
        }
    }

    pub fn icon_path(&self, cx: &mut App) -> SharedString {
        match self {
            MentionUri::File { abs_path } => {
                FileIcons::get_icon(abs_path, cx).unwrap_or_else(|| IconName::File.path().into())
            }
            MentionUri::PastedImage => IconName::Image.path().into(),
            MentionUri::Directory { .. } => FileIcons::get_folder_icon(false, cx)
                .unwrap_or_else(|| IconName::Folder.path().into()),
            MentionUri::Symbol { .. } => IconName::Code.path().into(),
            MentionUri::Thread { .. } => IconName::Thread.path().into(),
            MentionUri::TextThread { .. } => IconName::Thread.path().into(),
            MentionUri::Rule { .. } => IconName::Reader.path().into(),
            MentionUri::Selection { .. } => IconName::Reader.path().into(),
            MentionUri::Fetch { .. } => IconName::ToolWeb.path().into(),
        }
    }

    pub fn as_link<'a>(&'a self) -> MentionLink<'a> {
        MentionLink(self)
    }

    pub fn to_uri(&self) -> Url {
        match self {
            MentionUri::File { abs_path } => {
                Url::from_file_path(abs_path).expect("mention path should be absolute")
            }
            MentionUri::PastedImage => Url::parse("zed:///agent/pasted-image").unwrap(),
            MentionUri::Directory { abs_path } => {
                Url::from_directory_path(abs_path).expect("mention path should be absolute")
            }
            MentionUri::Symbol {
                abs_path,
                name,
                line_range,
            } => {
                let mut url =
                    Url::from_file_path(abs_path).expect("mention path should be absolute");
                url.query_pairs_mut().append_pair("symbol", name);
                url.set_fragment(Some(&format!(
                    "L{}:{}",
                    line_range.start() + 1,
                    line_range.end() + 1
                )));
                url
            }
            MentionUri::Selection {
                abs_path: path,
                line_range,
            } => {
                let mut url = if let Some(path) = path {
                    Url::from_file_path(path).expect("mention path should be absolute")
                } else {
                    let mut url = Url::parse("zed:///").unwrap();
                    url.set_path("/agent/untitled-buffer");
                    url
                };
                url.set_fragment(Some(&format!(
                    "L{}:{}",
                    line_range.start() + 1,
                    line_range.end() + 1
                )));
                url
            }
            MentionUri::Thread { name, id } => {
                let mut url = Url::parse("zed:///").unwrap();
                url.set_path(&format!("/agent/thread/{id}"));
                url.query_pairs_mut().append_pair("name", name);
                url
            }
            MentionUri::TextThread { path, name } => {
                let mut url = Url::parse("zed:///").unwrap();
                url.set_path(&format!(
                    "/agent/text-thread/{}",
                    path.to_string_lossy().trim_start_matches('/')
                ));
                url.query_pairs_mut().append_pair("name", name);
                url
            }
            MentionUri::Rule { name, id } => {
                let mut url = Url::parse("zed:///").unwrap();
                url.set_path(&format!("/agent/rule/{id}"));
                url.query_pairs_mut().append_pair("name", name);
                url
            }
            MentionUri::Fetch { url } => url.clone(),
        }
    }
}

impl FromStr for MentionUri {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        Self::parse(s)
    }
}

pub struct MentionLink<'a>(&'a MentionUri);

impl fmt::Display for MentionLink<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[@{}]({})", self.0.name(), self.0.to_uri())
    }
}

fn single_query_param(url: &Url, name: &'static str) -> Result<Option<String>> {
    let pairs = url.query_pairs().collect::<Vec<_>>();
    match pairs.as_slice() {
        [] => Ok(None),
        [(k, v)] => {
            if k != name {
                bail!("invalid query parameter")
            }

            Ok(Some(v.to_string()))
        }
        _ => bail!("too many query pairs"),
    }
}

pub fn selection_name(path: Option<&Path>, line_range: &RangeInclusive<u32>) -> String {
    format!(
        "{} ({}:{})",
        path.and_then(|path| path.file_name())
            .unwrap_or("Untitled".as_ref())
            .display(),
        *line_range.start() + 1,
        *line_range.end() + 1
    )
}
