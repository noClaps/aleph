use action_log::ActionLog;
use agent_client_protocol::{self as acp, ToolCallUpdateFields};
use anyhow::{Context as _, Result, anyhow};
use assistant_tool::outline;
use gpui::{App, Entity, SharedString, Task};
use indoc::formatdoc;
use language::Point;
use language_model::{LanguageModelImage, LanguageModelToolResultContent};
use project::{AgentLocation, ImageItem, Project, WorktreeSettings, image_store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings;
use std::{path::Path, sync::Arc};
use util::markdown::MarkdownCodeBlock;

use crate::{AgentTool, ToolCallEventStream};

/// Reads the content of the given file in the project.
///
/// - Never attempt to read a path that hasn't been previously mentioned.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileToolInput {
    /// The relative path of the file to read.
    ///
    /// This path should never be absolute, and the first component of the path should always be a root directory in a project.
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

pub struct ReadFileTool {
    project: Entity<Project>,
    action_log: Entity<ActionLog>,
}

impl ReadFileTool {
    pub fn new(project: Entity<Project>, action_log: Entity<ActionLog>) -> Self {
        Self {
            project,
            action_log,
        }
    }
}

impl AgentTool for ReadFileTool {
    type Input = ReadFileToolInput;
    type Output = LanguageModelToolResultContent;

    fn name() -> &'static str {
        "read_file"
    }

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(&self, input: Result<Self::Input, serde_json::Value>) -> SharedString {
        input
            .ok()
            .as_ref()
            .and_then(|input| Path::new(&input.path).file_name())
            .map(|file_name| file_name.to_string_lossy().to_string().into())
            .unwrap_or_default()
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<LanguageModelToolResultContent>> {
        let Some(project_path) = self.project.read(cx).find_project_path(&input.path, cx) else {
            return Task::ready(Err(anyhow!("Path {} not found in project", &input.path)));
        };

        // Error out if this path is either excluded or private in global settings
        let global_settings = WorktreeSettings::get_global(cx);
        if global_settings.is_path_excluded(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot read file because its path matches the global `file_scan_exclusions` setting: {}",
                &input.path
            )));
        }

        if global_settings.is_path_private(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot read file because its path matches the global `private_files` setting: {}",
                &input.path
            )));
        }

        // Error out if this path is either excluded or private in worktree settings
        let worktree_settings = WorktreeSettings::get(Some((&project_path).into()), cx);
        if worktree_settings.is_path_excluded(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot read file because its path matches the worktree `file_scan_exclusions` setting: {}",
                &input.path
            )));
        }

        if worktree_settings.is_path_private(&project_path.path) {
            return Task::ready(Err(anyhow!(
                "Cannot read file because its path matches the worktree `private_files` setting: {}",
                &input.path
            )));
        }

        let file_path = input.path.clone();

        if image_store::is_image_file(&self.project, &project_path, cx) {
            return cx.spawn(async move |cx| {
                let image_entity: Entity<ImageItem> = cx
                    .update(|cx| {
                        self.project.update(cx, |project, cx| {
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

                Ok(language_model_image.into())
            });
        }

        let project = self.project.clone();
        let action_log = self.action_log.clone();

        cx.spawn(async move |cx| {
            let buffer = cx
                .update(|cx| {
                    project.update(cx, |project, cx| {
                        project.open_buffer(project_path.clone(), cx)
                    })
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

            let mut anchor = None;

            // Check if specific line ranges are provided
            let result = if input.start_line.is_some() || input.end_line.is_some() {
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
                        itertools::intersperse(lines.take(count as usize), "\n").collect::<String>()
                    } else {
                        itertools::intersperse(lines, "\n").collect::<String>()
                    }
                })?;

                action_log.update(cx, |log, cx| {
                    log.buffer_read(buffer.clone(), cx);
                })?;

                Ok(result.into())
            } else {
                // No line ranges specified, so check file size to see if it's too big.
                let file_size = buffer.read_with(cx, |buffer, _cx| buffer.text().len())?;

                if file_size <= outline::AUTO_OUTLINE_SIZE {
                    // File is small enough, so return its contents.
                    let result = buffer.read_with(cx, |buffer, _cx| buffer.text())?;

                    action_log.update(cx, |log, cx| {
                        log.buffer_read(buffer.clone(), cx);
                    })?;

                    Ok(result.into())
                } else {
                    // File is too big, so return the outline
                    // and a suggestion to read again with line numbers.
                    let outline =
                        outline::file_outline(project.clone(), file_path, action_log, None, cx)
                            .await?;
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
            };

            project.update(cx, |project, cx| {
                if let Some(abs_path) = project.absolute_path(&project_path, cx) {
                    project.set_agent_location(
                        Some(AgentLocation {
                            buffer: buffer.downgrade(),
                            position: anchor.unwrap_or(text::Anchor::MIN),
                        }),
                        cx,
                    );
                    event_stream.update_fields(ToolCallUpdateFields {
                        locations: Some(vec![acp::ToolCallLocation {
                            path: abs_path,
                            line: input.start_line.map(|line| line.saturating_sub(1)),
                        }]),
                        ..Default::default()
                    });
                    if let Ok(LanguageModelToolResultContent::Text(text)) = &result {
                        let markdown = MarkdownCodeBlock {
                            tag: &input.path,
                            text,
                        }
                        .to_string();
                        event_stream.update_fields(ToolCallUpdateFields {
                            content: Some(vec![acp::ToolCallContent::Content {
                                content: markdown.into(),
                            }]),
                            ..Default::default()
                        })
                    }
                }
            })?;

            result
        })
    }
}
