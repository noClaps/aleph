use anyhow::bail;
use collections::HashMap;
use serde::Deserialize;
use util::ResultExt;

use crate::{EnvVariableReplacer, TaskTemplate, TaskTemplates, VariableName};

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct TaskOptions {
    cwd: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct VsCodeTaskDefinition {
    label: String,
    #[serde(flatten)]
    command: Option<Command>,
    #[serde(flatten)]
    other_attributes: HashMap<String, serde_json_lenient::Value>,
    options: Option<TaskOptions>,
}

#[derive(Clone, Deserialize, PartialEq, Debug)]
#[serde(tag = "type")]
#[serde(rename_all = "camelCase")]
enum Command {
    Npm {
        script: String,
    },
    Shell {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Gulp {
        task: String,
    },
}

impl VsCodeTaskDefinition {
    fn into_zed_format(
        self,
        replacer: &EnvVariableReplacer,
    ) -> anyhow::Result<Option<TaskTemplate>> {
        if self.other_attributes.contains_key("dependsOn") {
            log::warn!(
                "Skipping deserializing of a task `{}` with the unsupported `dependsOn` key",
                self.label
            );
            return Ok(None);
        }
        // `type` might not be set in e.g. tasks that use `dependsOn`; we still want to deserialize the whole object though (hence command is an Option),
        // as that way we can provide more specific description of why deserialization failed.
        // E.g. if the command is missing due to `dependsOn` presence, we can check other_attributes first before doing this (and provide nice error message)
        // catch-all if on value.command presence.
        let Some(command) = self.command else {
            bail!("Missing `type` field in task");
        };

        let (command, args) = match command {
            Command::Npm { script } => ("npm".to_owned(), vec!["run".to_string(), script]),
            Command::Shell { command, args } => (command, args),
            Command::Gulp { task } => ("gulp".to_owned(), vec![task]),
        };
        // Per VSC docs, only `command`, `args` and `options` support variable substitution.
        let command = replacer.replace(&command);
        let args = args.into_iter().map(|arg| replacer.replace(&arg)).collect();
        let mut template = TaskTemplate {
            label: self.label,
            command,
            args,
            ..TaskTemplate::default()
        };
        if let Some(options) = self.options {
            template.cwd = options.cwd.map(|cwd| replacer.replace(&cwd));
            template.env = options.env;
        }
        Ok(Some(template))
    }
}

/// [`VsCodeTaskFile`] is a superset of Code's task definition format.
#[derive(Debug, Deserialize, PartialEq)]
pub struct VsCodeTaskFile {
    tasks: Vec<VsCodeTaskDefinition>,
}

impl TryFrom<VsCodeTaskFile> for TaskTemplates {
    type Error = anyhow::Error;

    fn try_from(value: VsCodeTaskFile) -> Result<Self, Self::Error> {
        let replacer = EnvVariableReplacer::new(HashMap::from_iter([
            (
                "workspaceFolder".to_owned(),
                VariableName::WorktreeRoot.to_string(),
            ),
            ("file".to_owned(), VariableName::File.to_string()),
            ("lineNumber".to_owned(), VariableName::Row.to_string()),
            (
                "selectedText".to_owned(),
                VariableName::SelectedText.to_string(),
            ),
        ]));
        let templates = value
            .tasks
            .into_iter()
            .filter_map(|vscode_definition| {
                vscode_definition
                    .into_zed_format(&replacer)
                    .log_err()
                    .flatten()
            })
            .collect();
        Ok(Self(templates))
    }
}
