mod extension_slash_command;
mod slash_command_registry;
mod slash_command_working_set;

pub use crate::extension_slash_command::*;
pub use crate::slash_command_registry::*;
pub use crate::slash_command_working_set::*;
use anyhow::Result;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use gpui::{App, SharedString, Task, WeakEntity, Window};
use language::HighlightId;
use language::{BufferSnapshot, CodeLabel, LspAdapterDelegate, OffsetRangeExt};
pub use language_model::Role;
use serde::{Deserialize, Serialize};
use std::{
    ops::Range,
    sync::{Arc, atomic::AtomicBool},
};
use ui::ActiveTheme;
use workspace::{Workspace, ui::IconName};

pub fn init(cx: &mut App) {
    SlashCommandRegistry::default_global(cx);
    extension_slash_command::init(cx);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterCompletion {
    /// Run the command
    Run,
    /// Continue composing the current argument, doesn't add a space
    Compose,
    /// Continue the command composition, adds a space
    Continue,
}

impl From<bool> for AfterCompletion {
    fn from(value: bool) -> Self {
        if value {
            AfterCompletion::Run
        } else {
            AfterCompletion::Continue
        }
    }
}

impl AfterCompletion {
    pub fn run(&self) -> bool {
        match self {
            AfterCompletion::Run => true,
            AfterCompletion::Compose | AfterCompletion::Continue => false,
        }
    }
}

#[derive(Debug)]
pub struct ArgumentCompletion {
    /// The label to display for this completion.
    pub label: CodeLabel,
    /// The new text that should be inserted into the command when this completion is accepted.
    pub new_text: String,
    /// Whether the command should be run when accepting this completion.
    pub after_completion: AfterCompletion,
    /// Whether to replace the all arguments, or whether to treat this as an independent argument.
    pub replace_previous_arguments: bool,
}

pub type SlashCommandResult = Result<BoxStream<'static, Result<SlashCommandEvent>>>;

pub trait SlashCommand: 'static + Send + Sync {
    fn name(&self) -> String;
    fn icon(&self) -> IconName {
        IconName::Slash
    }
    fn label(&self, _cx: &App) -> CodeLabel {
        CodeLabel::plain(self.name(), None)
    }
    fn description(&self) -> String;
    fn menu_text(&self) -> String;
    fn complete_argument(
        self: Arc<Self>,
        arguments: &[String],
        cancel: Arc<AtomicBool>,
        workspace: Option<WeakEntity<Workspace>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<ArgumentCompletion>>>;
    fn requires_argument(&self) -> bool;
    fn accepts_arguments(&self) -> bool {
        self.requires_argument()
    }
    fn run(
        self: Arc<Self>,
        arguments: &[String],
        context_slash_command_output_sections: &[SlashCommandOutputSection<language::Anchor>],
        context_buffer: BufferSnapshot,
        workspace: WeakEntity<Workspace>,
        // TODO: We're just using the `LspAdapterDelegate` here because that is
        // what the extension API is already expecting.
        //
        // It may be that `LspAdapterDelegate` needs a more general name, or
        // perhaps another kind of delegate is needed here.
        delegate: Option<Arc<dyn LspAdapterDelegate>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<SlashCommandResult>;
}

#[derive(Debug, PartialEq)]
pub enum SlashCommandContent {
    Text {
        text: String,
        run_commands_in_text: bool,
    },
}

impl<'a> From<&'a str> for SlashCommandContent {
    fn from(text: &'a str) -> Self {
        Self::Text {
            text: text.into(),
            run_commands_in_text: false,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum SlashCommandEvent {
    StartMessage {
        role: Role,
        merge_same_roles: bool,
    },
    StartSection {
        icon: IconName,
        label: SharedString,
        metadata: Option<serde_json::Value>,
    },
    Content(SlashCommandContent),
    EndSection,
}

#[derive(Debug, Default, PartialEq, Clone)]
pub struct SlashCommandOutput {
    pub text: String,
    pub sections: Vec<SlashCommandOutputSection<usize>>,
    pub run_commands_in_text: bool,
}

impl SlashCommandOutput {
    pub fn ensure_valid_section_ranges(&mut self) {
        for section in &mut self.sections {
            section.range.start = section.range.start.min(self.text.len());
            section.range.end = section.range.end.min(self.text.len());
            while !self.text.is_char_boundary(section.range.start) {
                section.range.start -= 1;
            }
            while !self.text.is_char_boundary(section.range.end) {
                section.range.end += 1;
            }
        }
    }

    /// Returns this [`SlashCommandOutput`] as a stream of [`SlashCommandEvent`]s.
    pub fn into_event_stream(mut self) -> BoxStream<'static, Result<SlashCommandEvent>> {
        self.ensure_valid_section_ranges();

        let mut events = Vec::new();

        let mut section_endpoints = Vec::new();
        for section in self.sections {
            section_endpoints.push((
                section.range.start,
                SlashCommandEvent::StartSection {
                    icon: section.icon,
                    label: section.label,
                    metadata: section.metadata,
                },
            ));
            section_endpoints.push((section.range.end, SlashCommandEvent::EndSection));
        }
        section_endpoints.sort_by_key(|(offset, _)| *offset);

        let mut content_offset = 0;
        for (endpoint_offset, endpoint) in section_endpoints {
            if content_offset < endpoint_offset {
                events.push(Ok(SlashCommandEvent::Content(SlashCommandContent::Text {
                    text: self.text[content_offset..endpoint_offset].to_string(),
                    run_commands_in_text: self.run_commands_in_text,
                })));
                content_offset = endpoint_offset;
            }

            events.push(Ok(endpoint));
        }

        if content_offset < self.text.len() {
            events.push(Ok(SlashCommandEvent::Content(SlashCommandContent::Text {
                text: self.text[content_offset..].to_string(),
                run_commands_in_text: self.run_commands_in_text,
            })));
        }

        stream::iter(events).boxed()
    }

    pub async fn from_event_stream(
        mut events: BoxStream<'static, Result<SlashCommandEvent>>,
    ) -> Result<SlashCommandOutput> {
        let mut output = SlashCommandOutput::default();
        let mut section_stack = Vec::new();

        while let Some(event) = events.next().await {
            match event? {
                SlashCommandEvent::StartSection {
                    icon,
                    label,
                    metadata,
                } => {
                    let start = output.text.len();
                    section_stack.push(SlashCommandOutputSection {
                        range: start..start,
                        icon,
                        label,
                        metadata,
                    });
                }
                SlashCommandEvent::Content(SlashCommandContent::Text {
                    text,
                    run_commands_in_text,
                }) => {
                    output.text.push_str(&text);
                    output.run_commands_in_text = run_commands_in_text;

                    if let Some(section) = section_stack.last_mut() {
                        section.range.end = output.text.len();
                    }
                }
                SlashCommandEvent::EndSection => {
                    if let Some(section) = section_stack.pop() {
                        output.sections.push(section);
                    }
                }
                SlashCommandEvent::StartMessage { .. } => {}
            }
        }

        while let Some(section) = section_stack.pop() {
            output.sections.push(section);
        }

        Ok(output)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommandOutputSection<T> {
    pub range: Range<T>,
    pub icon: IconName,
    pub label: SharedString,
    pub metadata: Option<serde_json::Value>,
}

impl SlashCommandOutputSection<language::Anchor> {
    pub fn is_valid(&self, buffer: &language::TextBuffer) -> bool {
        self.range.start.is_valid(buffer) && !self.range.to_offset(buffer).is_empty()
    }
}

pub struct SlashCommandLine {
    /// The range within the line containing the command name.
    pub name: Range<usize>,
    /// Ranges within the line containing the command arguments.
    pub arguments: Vec<Range<usize>>,
}

impl SlashCommandLine {
    pub fn parse(line: &str) -> Option<Self> {
        let mut call: Option<Self> = None;
        let mut ix = 0;
        for c in line.chars() {
            let next_ix = ix + c.len_utf8();
            if let Some(call) = &mut call {
                // The command arguments start at the first non-whitespace character
                // after the command name, and continue until the end of the line.
                if let Some(argument) = call.arguments.last_mut() {
                    if c.is_whitespace() {
                        if (*argument).is_empty() {
                            argument.start = next_ix;
                            argument.end = next_ix;
                        } else {
                            argument.end = ix;
                            call.arguments.push(next_ix..next_ix);
                        }
                    } else {
                        argument.end = next_ix;
                    }
                }
                // The command name ends at the first whitespace character.
                else if !call.name.is_empty() {
                    if c.is_whitespace() {
                        call.arguments = vec![next_ix..next_ix];
                    } else {
                        call.name.end = next_ix;
                    }
                }
                // The command name must begin with a letter.
                else if c.is_alphabetic() {
                    call.name.end = next_ix;
                } else {
                    return None;
                }
            }
            // Commands start with a slash.
            else if c == '/' {
                call = Some(SlashCommandLine {
                    name: next_ix..next_ix,
                    arguments: Vec::new(),
                });
            }
            // The line can't contain anything before the slash except for whitespace.
            else if !c.is_whitespace() {
                return None;
            }
            ix = next_ix;
        }
        call
    }
}

pub fn create_label_for_command(command_name: &str, arguments: &[&str], cx: &App) -> CodeLabel {
    let mut label = CodeLabel::default();
    label.push_str(command_name, None);
    label.push_str(" ", None);
    label.push_str(
        &arguments.join(" "),
        cx.theme().syntax().highlight_id("comment").map(HighlightId),
    );
    label.filter_range = 0..command_name.len();
    label
}
