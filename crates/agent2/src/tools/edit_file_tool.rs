use crate::{AgentTool, Thread, ToolCallEventStream};
use acp_thread::Diff;
use agent_client_protocol::{self as acp, ToolCallLocation, ToolCallUpdateFields};
use anyhow::{Context as _, Result, anyhow};
use assistant_tools::edit_agent::{EditAgent, EditAgentOutput, EditAgentOutputEvent, EditFormat};
use cloud_llm_client::CompletionIntent;
use collections::HashSet;
use gpui::{App, AppContext, AsyncApp, Entity, Task, WeakEntity};
use indoc::formatdoc;
use language::language_settings::{self, FormatOnSave};
use language::{LanguageRegistry, ToPoint};
use language_model::LanguageModelToolResultContent;
use paths;
use project::lsp_store::{FormatTrigger, LspFormatTarget};
use project::{Project, ProjectPath};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings;
use smol::stream::StreamExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ui::SharedString;
use util::ResultExt;

const DEFAULT_UI_TEXT: &str = "Editing file";

/// This is a tool for creating a new file or editing an existing file. For moving or renaming files, you should generally use the `terminal` tool with the 'mv' command instead.
///
/// Before using this tool:
///
/// 1. Use the `read_file` tool to understand the file's contents and context
///
/// 2. Verify the directory path is correct (only applicable when creating new files):
///    - Use the `list_directory` tool to verify the parent directory exists and is the correct location
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EditFileToolInput {
    /// A one-line, user-friendly markdown description of the edit. This will be shown in the UI and also passed to another model to perform the edit.
    ///
    /// Be terse, but also descriptive in what you want to achieve with this edit. Avoid generic instructions.
    ///
    /// NEVER mention the file path in this description.
    ///
    /// <example>Fix API endpoint URLs</example>
    /// <example>Update copyright year in `page_footer`</example>
    ///
    /// Make sure to include this field before all the others in the input object so that we can display it immediately.
    pub display_description: String,

    /// The full path of the file to create or modify in the project.
    ///
    /// WARNING: When specifying which file path need changing, you MUST start each path with one of the project's root directories.
    ///
    /// The following examples assume we have two root directories in the project:
    /// - /a/b/backend
    /// - /c/d/frontend
    ///
    /// <example>
    /// `backend/src/main.rs`
    ///
    /// Notice how the file path starts with `backend`. Without that, the path would be ambiguous and the call would fail!
    /// </example>
    ///
    /// <example>
    /// `frontend/db.js`
    /// </example>
    pub path: PathBuf,
    /// The mode of operation on the file. Possible values:
    /// - 'edit': Make granular edits to an existing file.
    /// - 'create': Create a new file if it doesn't exist.
    /// - 'overwrite': Replace the entire contents of an existing file.
    ///
    /// When a file already exists or you just created it, prefer editing it as opposed to recreating it from scratch.
    pub mode: EditFileMode,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EditFileToolPartialInput {
    #[serde(default)]
    path: String,
    #[serde(default)]
    display_description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(inline)]
pub enum EditFileMode {
    Edit,
    Create,
    Overwrite,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EditFileToolOutput {
    #[serde(alias = "original_path")]
    input_path: PathBuf,
    new_text: String,
    old_text: Arc<String>,
    #[serde(default)]
    diff: String,
    #[serde(alias = "raw_output")]
    edit_agent_output: EditAgentOutput,
}

impl From<EditFileToolOutput> for LanguageModelToolResultContent {
    fn from(output: EditFileToolOutput) -> Self {
        if output.diff.is_empty() {
            "No edits were made.".into()
        } else {
            format!(
                "Edited {}:\n\n```diff\n{}\n```",
                output.input_path.display(),
                output.diff
            )
            .into()
        }
    }
}

pub struct EditFileTool {
    thread: WeakEntity<Thread>,
    language_registry: Arc<LanguageRegistry>,
}

impl EditFileTool {
    pub fn new(thread: WeakEntity<Thread>, language_registry: Arc<LanguageRegistry>) -> Self {
        Self {
            thread,
            language_registry,
        }
    }

    fn authorize(
        &self,
        input: &EditFileToolInput,
        event_stream: &ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<()>> {
        if agent_settings::AgentSettings::get_global(cx).always_allow_tool_actions {
            return Task::ready(Ok(()));
        }

        // If any path component matches the local settings folder, then this could affect
        // the editor in ways beyond the project source, so prompt.
        let local_settings_folder = paths::local_settings_folder_relative_path();
        let path = Path::new(&input.path);
        if path
            .components()
            .any(|component| component.as_os_str() == local_settings_folder.as_os_str())
        {
            return event_stream.authorize(
                format!("{} (local settings)", input.display_description),
                cx,
            );
        }

        // It's also possible that the global config dir is configured to be inside the project,
        // so check for that edge case too.
        if let Ok(canonical_path) = std::fs::canonicalize(&input.path)
            && canonical_path.starts_with(paths::config_dir())
        {
            return event_stream.authorize(
                format!("{} (global settings)", input.display_description),
                cx,
            );
        }

        // Check if path is inside the global config directory
        // First check if it's already inside project - if not, try to canonicalize
        let Ok(project_path) = self.thread.read_with(cx, |thread, cx| {
            thread.project().read(cx).find_project_path(&input.path, cx)
        }) else {
            return Task::ready(Err(anyhow!("thread was dropped")));
        };

        // If the path is inside the project, and it's not one of the above edge cases,
        // then no confirmation is necessary. Otherwise, confirmation is necessary.
        if project_path.is_some() {
            Task::ready(Ok(()))
        } else {
            event_stream.authorize(&input.display_description, cx)
        }
    }
}

impl AgentTool for EditFileTool {
    type Input = EditFileToolInput;
    type Output = EditFileToolOutput;

    fn name() -> &'static str {
        "edit_file"
    }

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Edit
    }

    fn initial_title(&self, input: Result<Self::Input, serde_json::Value>) -> SharedString {
        match input {
            Ok(input) => input.display_description.into(),
            Err(raw_input) => {
                if let Some(input) =
                    serde_json::from_value::<EditFileToolPartialInput>(raw_input).ok()
                {
                    let description = input.display_description.trim();
                    if !description.is_empty() {
                        return description.to_string().into();
                    }

                    let path = input.path.trim().to_string();
                    if !path.is_empty() {
                        return path.into();
                    }
                }

                DEFAULT_UI_TEXT.into()
            }
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let Ok(project) = self
            .thread
            .read_with(cx, |thread, _cx| thread.project().clone())
        else {
            return Task::ready(Err(anyhow!("thread was dropped")));
        };
        let project_path = match resolve_path(&input, project.clone(), cx) {
            Ok(path) => path,
            Err(err) => return Task::ready(Err(anyhow!(err))),
        };
        let abs_path = project.read(cx).absolute_path(&project_path, cx);
        if let Some(abs_path) = abs_path.clone() {
            event_stream.update_fields(ToolCallUpdateFields {
                locations: Some(vec![acp::ToolCallLocation {
                    path: abs_path,
                    line: None,
                }]),
                ..Default::default()
            });
        }

        let authorize = self.authorize(&input, &event_stream, cx);
        cx.spawn(async move |cx: &mut AsyncApp| {
            authorize.await?;

            let (request, model, action_log) = self.thread.update(cx, |thread, cx| {
                let request = thread.build_completion_request(CompletionIntent::ToolResults, cx);
                (request, thread.model().cloned(), thread.action_log().clone())
            })?;
            let request = request?;
            let model = model.context("No language model configured")?;

            let edit_format = EditFormat::from_model(model.clone())?;
            let edit_agent = EditAgent::new(
                model,
                project.clone(),
                action_log.clone(),
                // TODO: move edit agent to this crate so we can use our templates
                assistant_tools::templates::Templates::new(),
                edit_format,
            );

            let buffer = project
                .update(cx, |project, cx| {
                    project.open_buffer(project_path.clone(), cx)
                })?
                .await?;

            let diff = cx.new(|cx| Diff::new(buffer.clone(), cx))?;
            event_stream.update_diff(diff.clone());
            let _finalize_diff = util::defer({
               let diff = diff.downgrade();
               let mut cx = cx.clone();
               move || {
                   diff.update(&mut cx, |diff, cx| diff.finalize(cx)).ok();
               }
            });

            let old_snapshot = buffer.read_with(cx, |buffer, _cx| buffer.snapshot())?;
            let old_text = cx
                .background_spawn({
                    let old_snapshot = old_snapshot.clone();
                    async move { Arc::new(old_snapshot.text()) }
                })
                .await;


            let (output, mut events) = if matches!(input.mode, EditFileMode::Edit) {
                edit_agent.edit(
                    buffer.clone(),
                    input.display_description.clone(),
                    &request,
                    cx,
                )
            } else {
                edit_agent.overwrite(
                    buffer.clone(),
                    input.display_description.clone(),
                    &request,
                    cx,
                )
            };

            let mut hallucinated_old_text = false;
            let mut ambiguous_ranges = Vec::new();
            let mut emitted_location = false;
            while let Some(event) = events.next().await {
                match event {
                    EditAgentOutputEvent::Edited(range) => {
                        if !emitted_location {
                            let line = buffer.update(cx, |buffer, _cx| {
                                range.start.to_point(&buffer.snapshot()).row
                            }).ok();
                            if let Some(abs_path) = abs_path.clone() {
                                event_stream.update_fields(ToolCallUpdateFields {
                                    locations: Some(vec![ToolCallLocation { path: abs_path, line }]),
                                    ..Default::default()
                                });
                            }
                            emitted_location = true;
                        }
                    },
                    EditAgentOutputEvent::UnresolvedEditRange => hallucinated_old_text = true,
                    EditAgentOutputEvent::AmbiguousEditRange(ranges) => ambiguous_ranges = ranges,
                    EditAgentOutputEvent::ResolvingEditRange(range) => {
                        diff.update(cx, |card, cx| card.reveal_range(range.clone(), cx))?;
                        // if !emitted_location {
                        //     let line = buffer.update(cx, |buffer, _cx| {
                        //         range.start.to_point(&buffer.snapshot()).row
                        //     }).ok();
                        //     if let Some(abs_path) = abs_path.clone() {
                        //         event_stream.update_fields(ToolCallUpdateFields {
                        //             locations: Some(vec![ToolCallLocation { path: abs_path, line }]),
                        //             ..Default::default()
                        //         });
                        //     }
                        // }
                    }
                }
            }

            // If format_on_save is enabled, format the buffer
            let format_on_save_enabled = buffer
                .read_with(cx, |buffer, cx| {
                    let settings = language_settings::language_settings(
                        buffer.language().map(|l| l.name()),
                        buffer.file(),
                        cx,
                    );
                    settings.format_on_save != FormatOnSave::Off
                })
                .unwrap_or(false);

            let edit_agent_output = output.await?;

            if format_on_save_enabled {
                action_log.update(cx, |log, cx| {
                    log.buffer_edited(buffer.clone(), cx);
                })?;

                let format_task = project.update(cx, |project, cx| {
                    project.format(
                        HashSet::from_iter([buffer.clone()]),
                        LspFormatTarget::Buffers,
                        false, // Don't push to history since the tool did it.
                        FormatTrigger::Save,
                        cx,
                    )
                })?;
                format_task.await.log_err();
            }

            project
                .update(cx, |project, cx| project.save_buffer(buffer.clone(), cx))?
                .await?;

            action_log.update(cx, |log, cx| {
                log.buffer_edited(buffer.clone(), cx);
            })?;

            let new_snapshot = buffer.read_with(cx, |buffer, _cx| buffer.snapshot())?;
            let (new_text, unified_diff) = cx
                .background_spawn({
                    let new_snapshot = new_snapshot.clone();
                    let old_text = old_text.clone();
                    async move {
                        let new_text = new_snapshot.text();
                        let diff = language::unified_diff(&old_text, &new_text);
                        (new_text, diff)
                    }
                })
                .await;

            let input_path = input.path.display();
            if unified_diff.is_empty() {
                anyhow::ensure!(
                    !hallucinated_old_text,
                    formatdoc! {"
                        Some edits were produced but none of them could be applied.
                        Read the relevant sections of {input_path} again so that
                        I can perform the requested edits.
                    "}
                );
                anyhow::ensure!(
                    ambiguous_ranges.is_empty(),
                    {
                        let line_numbers = ambiguous_ranges
                            .iter()
                            .map(|range| range.start.to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        formatdoc! {"
                            <old_text> matches more than one position in the file (lines: {line_numbers}). Read the
                            relevant sections of {input_path} again and extend <old_text> so
                            that I can perform the requested edits.
                        "}
                    }
                );
            }

            Ok(EditFileToolOutput {
                input_path: input.path,
                new_text,
                old_text,
                diff: unified_diff,
                edit_agent_output,
            })
        })
    }

    fn replay(
        &self,
        _input: Self::Input,
        output: Self::Output,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Result<()> {
        event_stream.update_diff(cx.new(|cx| {
            Diff::finalized(
                output.input_path,
                Some(output.old_text.to_string()),
                output.new_text,
                self.language_registry.clone(),
                cx,
            )
        }));
        Ok(())
    }
}

/// Validate that the file path is valid, meaning:
///
/// - For `edit` and `overwrite`, the path must point to an existing file.
/// - For `create`, the file must not already exist, but it's parent dir must exist.
fn resolve_path(
    input: &EditFileToolInput,
    project: Entity<Project>,
    cx: &mut App,
) -> Result<ProjectPath> {
    let project = project.read(cx);

    match input.mode {
        EditFileMode::Edit | EditFileMode::Overwrite => {
            let path = project
                .find_project_path(&input.path, cx)
                .context("Can't edit file: path not found")?;

            let entry = project
                .entry_for_path(&path, cx)
                .context("Can't edit file: path not found")?;

            anyhow::ensure!(entry.is_file(), "Can't edit file: path is a directory");
            Ok(path)
        }

        EditFileMode::Create => {
            if let Some(path) = project.find_project_path(&input.path, cx) {
                anyhow::ensure!(
                    project.entry_for_path(&path, cx).is_none(),
                    "Can't create file: file already exists"
                );
            }

            let parent_path = input
                .path
                .parent()
                .context("Can't create file: incorrect path")?;

            let parent_project_path = project.find_project_path(&parent_path, cx);

            let parent_entry = parent_project_path
                .as_ref()
                .and_then(|path| project.entry_for_path(path, cx))
                .context("Can't create file: parent directory doesn't exist")?;

            anyhow::ensure!(
                parent_entry.is_dir(),
                "Can't create file: parent is not a directory"
            );

            let file_name = input
                .path
                .file_name()
                .context("Can't create file: invalid filename")?;

            let new_file_path = parent_project_path.map(|parent| ProjectPath {
                path: Arc::from(parent.path.join(file_name)),
                ..parent
            });

            new_file_path.context("Can't create file")
        }
    }
}
