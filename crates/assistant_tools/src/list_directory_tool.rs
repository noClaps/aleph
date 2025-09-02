use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Result, anyhow};
use assistant_tool::{Tool, ToolResult};
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use project::{Project, WorktreeSettings};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings;
use std::{fmt::Write, path::Path, sync::Arc};
use ui::IconName;
use util::markdown::MarkdownInlineCode;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListDirectoryToolInput {
    /// The fully-qualified path of the directory to list in the project.
    ///
    /// This path should never be absolute, and the first component
    /// of the path should always be a root directory in a project.
    ///
    /// <example>
    /// If the project has the following root directories:
    ///
    /// - directory1
    /// - directory2
    ///
    /// You can list the contents of `directory1` by using the path `directory1`.
    /// </example>
    ///
    /// <example>
    /// If the project has the following root directories:
    ///
    /// - foo
    /// - bar
    ///
    /// If you wanna list contents in the directory `foo/baz`, you should use the path `foo/baz`.
    /// </example>
    pub path: String,
}

pub struct ListDirectoryTool;

impl Tool for ListDirectoryTool {
    fn name(&self) -> String {
        "list_directory".into()
    }

    fn needs_confirmation(&self, _: &serde_json::Value, _: &Entity<Project>, _: &App) -> bool {
        false
    }

    fn may_perform_edits(&self) -> bool {
        false
    }

    fn description(&self) -> String {
        include_str!("./list_directory_tool/description.md").into()
    }

    fn icon(&self) -> IconName {
        IconName::ToolFolder
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<ListDirectoryToolInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        match serde_json::from_value::<ListDirectoryToolInput>(input.clone()) {
            Ok(input) => {
                let path = MarkdownInlineCode(&input.path);
                format!("List the {path} directory's contents")
            }
            Err(_) => "List directory".to_string(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        _request: Arc<LanguageModelRequest>,
        project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        _model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        let input = match serde_json::from_value::<ListDirectoryToolInput>(input) {
            Ok(input) => input,
            Err(err) => return Task::ready(Err(anyhow!(err))).into(),
        };

        // Sometimes models will return these even though we tell it to give a path and not a glob.
        // When this happens, just list the root worktree directories.
        if matches!(input.path.as_str(), "." | "" | "./" | "*") {
            let output = project
                .read(cx)
                .worktrees(cx)
                .filter_map(|worktree| {
                    worktree.read(cx).root_entry().and_then(|entry| {
                        if entry.is_dir() {
                            entry.path.to_str()
                        } else {
                            None
                        }
                    })
                })
                .collect::<Vec<_>>()
                .join("\n");

            return Task::ready(Ok(output.into())).into();
        }

        let Some(project_path) = project.read(cx).find_project_path(&input.path, cx) else {
            return Task::ready(Err(anyhow!("Path {} not found in project", input.path))).into();
        };
        let Some(worktree) = project
            .read(cx)
            .worktree_for_id(project_path.worktree_id, cx)
        else {
            return Task::ready(Err(anyhow!("Worktree not found"))).into();
        };

        // Check if the directory whose contents we're listing is itself excluded or private
        let global_settings = WorktreeSettings::get_global(cx);
        if global_settings.is_path_excluded(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot list directory because its path matches the user's global `file_scan_exclusions` setting: {}",
                &input.path
            )))
            .into();
        }

        if global_settings.is_path_private(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot list directory because its path matches the user's global `private_files` setting: {}",
                &input.path
            )))
            .into();
        }

        let worktree_settings = WorktreeSettings::get(Some((&project_path).into()), cx);
        if worktree_settings.is_path_excluded(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot list directory because its path matches the user's worktree`file_scan_exclusions` setting: {}",
                &input.path
            )))
            .into();
        }

        if worktree_settings.is_path_private(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot list directory because its path matches the user's worktree `private_paths` setting: {}",
                &input.path
            )))
            .into();
        }

        let worktree_snapshot = worktree.read(cx).snapshot();
        let worktree_root_name = worktree.read(cx).root_name().to_string();

        let Some(entry) = worktree_snapshot.entry_for_path(&project_path.path) else {
            return Task::ready(Err(anyhow!("Path not found: {}", input.path))).into();
        };

        if !entry.is_dir() {
            return Task::ready(Err(anyhow!("{} is not a directory.", input.path))).into();
        }
        let worktree_snapshot = worktree.read(cx).snapshot();

        let mut folders = Vec::new();
        let mut files = Vec::new();

        for entry in worktree_snapshot.child_entries(&project_path.path) {
            // Skip private and excluded files and directories
            if global_settings.is_path_private(&entry.path)
                || global_settings.is_path_excluded(&entry.path)
            {
                continue;
            }

            if project
                .read(cx)
                .find_project_path(&entry.path, cx)
                .map(|project_path| {
                    let worktree_settings = WorktreeSettings::get(Some((&project_path).into()), cx);

                    worktree_settings.is_path_excluded(&project_path.path)
                        || worktree_settings.is_path_private(&project_path.path)
                })
                .unwrap_or(false)
            {
                continue;
            }

            let full_path = Path::new(&worktree_root_name)
                .join(&entry.path)
                .display()
                .to_string();
            if entry.is_dir() {
                folders.push(full_path);
            } else {
                files.push(full_path);
            }
        }

        let mut output = String::new();

        if !folders.is_empty() {
            writeln!(output, "# Folders:\n{}", folders.join("\n")).unwrap();
        }

        if !files.is_empty() {
            writeln!(output, "\n# Files:\n{}", files.join("\n")).unwrap();
        }

        if output.is_empty() {
            writeln!(output, "{} is empty.", input.path).unwrap();
        }

        Task::ready(Ok(output.into())).into()
    }
}
