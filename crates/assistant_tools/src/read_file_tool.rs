use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Context as _, Result, anyhow};
use assistant_tool::{Tool, ToolResult};
use assistant_tool::{ToolResultContent, outline};
use gpui::{AnyWindowHandle, App, Entity, Task};
use project::{ImageItem, image_store};

use assistant_tool::ToolResultOutput;
use indoc::formatdoc;
use itertools::Itertools;
use language::{Anchor, Point};
use language_model::{
    LanguageModel, LanguageModelImage, LanguageModelRequest, LanguageModelToolSchemaFormat,
};
use project::{AgentLocation, Project, WorktreeSettings};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings;
use std::sync::Arc;
use ui::IconName;

/// If the model requests to read a file whose size exceeds this, then
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileToolInput {
    /// The relative path of the file to read.
    ///
    /// This path should never be absolute, and the first component
    /// of the path should always be a root directory in a project.
    ///
    /// <example>
    /// If the project has the following root directories:
    ///
    /// - /a/b/directory1
    /// - /c/d/directory2
    ///
    /// If you want to access `file.txt` in `directory1`, you should use the path `directory1/file.txt`.
    /// If you want to access `file.txt` in `directory2`, you should use the path `directory2/file.txt`.
    /// </example>
    pub path: String,

    /// Optional line number to start reading on (1-based index)
    #[serde(default)]
    pub start_line: Option<u32>,

    /// Optional line number to end reading on (1-based index, inclusive)
    #[serde(default)]
    pub end_line: Option<u32>,
}

pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn name(&self) -> String {
        "read_file".into()
    }

    fn needs_confirmation(&self, _: &serde_json::Value, _: &Entity<Project>, _: &App) -> bool {
        false
    }

    fn may_perform_edits(&self) -> bool {
        false
    }

    fn description(&self) -> String {
        include_str!("./read_file_tool/description.md").into()
    }

    fn icon(&self) -> IconName {
        IconName::ToolSearch
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<ReadFileToolInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        match serde_json::from_value::<ReadFileToolInput>(input.clone()) {
            Ok(input) => {
                let path = &input.path;
                match (input.start_line, input.end_line) {
                    (Some(start), Some(end)) => {
                        format!(
                            "[Read file `{}` (lines {}-{})](@selection:{}:({}-{}))",
                            path, start, end, path, start, end
                        )
                    }
                    (Some(start), None) => {
                        format!(
                            "[Read file `{}` (from line {})](@selection:{}:({}-{}))",
                            path, start, path, start, start
                        )
                    }
                    _ => format!("[Read file `{}`](@file:{})", path, path),
                }
            }
            Err(_) => "Read file".to_string(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        _request: Arc<LanguageModelRequest>,
        project: Entity<Project>,
        action_log: Entity<ActionLog>,
        model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        let input = match serde_json::from_value::<ReadFileToolInput>(input) {
            Ok(input) => input,
            Err(err) => return Task::ready(Err(anyhow!(err))).into(),
        };

        let Some(project_path) = project.read(cx).find_project_path(&input.path, cx) else {
            return Task::ready(Err(anyhow!("Path {} not found in project", &input.path))).into();
        };

        // Error out if this path is either excluded or private in global settings
        let global_settings = WorktreeSettings::get_global(cx);
        if global_settings.is_path_excluded(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot read file because its path matches the global `file_scan_exclusions` setting: {}",
                &input.path
            )))
            .into();
        }

        if global_settings.is_path_private(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot read file because its path matches the global `private_files` setting: {}",
                &input.path
            )))
            .into();
        }

        // Error out if this path is either excluded or private in worktree settings
        let worktree_settings = WorktreeSettings::get(Some((&project_path).into()), cx);
        if worktree_settings.is_path_excluded(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot read file because its path matches the worktree `file_scan_exclusions` setting: {}",
                &input.path
            )))
            .into();
        }

        if worktree_settings.is_path_private(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot read file because its path matches the worktree `private_files` setting: {}",
                &input.path
            )))
            .into();
        }

        let file_path = input.path.clone();

        if image_store::is_image_file(&project, &project_path, cx) {
            if !model.supports_images() {
                return Task::ready(Err(anyhow!(
                    "Attempted to read an image, but Zed doesn't currently support sending images to {}.",
                    model.name().0
                )))
                .into();
            }

            let task = cx.spawn(async move |cx| -> Result<ToolResultOutput> {
                let image_entity: Entity<ImageItem> = cx
                    .update(|cx| {
                        project.update(cx, |project, cx| {
                            project.open_image(project_path.clone(), cx)
                        })
                    })?
                    .await?;

                let image =
                    image_entity.read_with(cx, |image_item, _| Arc::clone(&image_item.image))?;

                let language_model_image = cx
                    .update(|cx| LanguageModelImage::from_image(image, cx))?
                    .await
                    .context("processing image")?;

                Ok(ToolResultOutput {
                    content: ToolResultContent::Image(language_model_image),
                    output: None,
                })
            });

            return task.into();
        }

        cx.spawn(async move |cx| {
            let buffer = cx
                .update(|cx| {
                    project.update(cx, |project, cx| project.open_buffer(project_path, cx))
                })?
                .await?;
            if buffer.read_with(cx, |buffer, _| {
                buffer
                    .file()
                    .as_ref()
                    .is_none_or(|file| !file.disk_state().exists())
            })? {
                anyhow::bail!("{file_path} not found");
            }

            project.update(cx, |project, cx| {
                project.set_agent_location(
                    Some(AgentLocation {
                        buffer: buffer.downgrade(),
                        position: Anchor::MIN,
                    }),
                    cx,
                );
            })?;

            // Check if specific line ranges are provided
            if input.start_line.is_some() || input.end_line.is_some() {
                let mut anchor = None;
                let result = buffer.read_with(cx, |buffer, _cx| {
                    let text = buffer.text();
                    // .max(1) because despite instructions to be 1-indexed, sometimes the model passes 0.
                    let start = input.start_line.unwrap_or(1).max(1);
                    let start_row = start - 1;
                    if start_row <= buffer.max_point().row {
                        let column = buffer.line_indent_for_row(start_row).raw_len();
                        anchor = Some(buffer.anchor_before(Point::new(start_row, column)));
                    }

                    let lines = text.split('\n').skip(start_row as usize);
                    if let Some(end) = input.end_line {
                        let count = end.saturating_sub(start).saturating_add(1); // Ensure at least 1 line
                        Itertools::intersperse(lines.take(count as usize), "\n")
                            .collect::<String>()
                            .into()
                    } else {
                        Itertools::intersperse(lines, "\n")
                            .collect::<String>()
                            .into()
                    }
                })?;

                action_log.update(cx, |log, cx| {
                    log.buffer_read(buffer.clone(), cx);
                })?;

                if let Some(anchor) = anchor {
                    project.update(cx, |project, cx| {
                        project.set_agent_location(
                            Some(AgentLocation {
                                buffer: buffer.downgrade(),
                                position: anchor,
                            }),
                            cx,
                        );
                    })?;
                }

                Ok(result)
            } else {
                // No line ranges specified, so check file size to see if it's too big.
                let file_size = buffer.read_with(cx, |buffer, _cx| buffer.text().len())?;

                if file_size <= outline::AUTO_OUTLINE_SIZE {
                    // File is small enough, so return its contents.
                    let result = buffer.read_with(cx, |buffer, _cx| buffer.text())?;

                    action_log.update(cx, |log, cx| {
                        log.buffer_read(buffer, cx);
                    })?;

                    Ok(result.into())
                } else {
                    // File is too big, so return the outline
                    // and a suggestion to read again with line numbers.
                    let outline =
                        outline::file_outline(project, file_path, action_log, None, cx).await?;
                    Ok(formatdoc! {"
                        This file was too big to read all at once.

                        Here is an outline of its symbols:

                        {outline}

                        Using the line numbers in this outline, you can call this tool again
                        while specifying the start_line and end_line fields to see the
                        implementations of symbols in the outline.

                        Alternatively, you can fall back to the `grep` tool (if available)
                        to search the file for specific content."
                    }
                    .into())
                }
            }
        })
        .into()
    }
}
