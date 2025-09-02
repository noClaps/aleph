use crate::{Completion, Copilot};
use anyhow::Result;
use edit_prediction::{Direction, EditPrediction, EditPredictionProvider};
use gpui::{App, Context, Entity, EntityId, Task};
use language::{Buffer, OffsetRangeExt, ToOffset, language_settings::AllLanguageSettings};
use project::Project;
use settings::Settings;
use std::{path::Path, time::Duration};

pub const COPILOT_DEBOUNCE_TIMEOUT: Duration = Duration::from_millis(75);

pub struct CopilotCompletionProvider {
    cycled: bool,
    buffer_id: Option<EntityId>,
    completions: Vec<Completion>,
    active_completion_index: usize,
    file_extension: Option<String>,
    pending_refresh: Option<Task<Result<()>>>,
    pending_cycling_refresh: Option<Task<Result<()>>>,
    copilot: Entity<Copilot>,
}

impl CopilotCompletionProvider {
    pub fn new(copilot: Entity<Copilot>) -> Self {
        Self {
            cycled: false,
            buffer_id: None,
            completions: Vec::new(),
            active_completion_index: 0,
            file_extension: None,
            pending_refresh: None,
            pending_cycling_refresh: None,
            copilot,
        }
    }

    fn active_completion(&self) -> Option<&Completion> {
        self.completions.get(self.active_completion_index)
    }

    fn push_completion(&mut self, new_completion: Completion) {
        for completion in &self.completions {
            if completion.text == new_completion.text && completion.range == new_completion.range {
                return;
            }
        }
        self.completions.push(new_completion);
    }
}

impl EditPredictionProvider for CopilotCompletionProvider {
    fn name() -> &'static str {
        "copilot"
    }

    fn display_name() -> &'static str {
        "Copilot"
    }

    fn show_completions_in_menu() -> bool {
        true
    }

    fn show_tab_accept_marker() -> bool {
        true
    }

    fn supports_jump_to_edit() -> bool {
        false
    }

    fn is_refreshing(&self) -> bool {
        self.pending_refresh.is_some() && self.completions.is_empty()
    }

    fn is_enabled(
        &self,
        _buffer: &Entity<Buffer>,
        _cursor_position: language::Anchor,
        cx: &App,
    ) -> bool {
        self.copilot.read(cx).status().is_authorized()
    }

    fn refresh(
        &mut self,
        _project: Option<Entity<Project>>,
        buffer: Entity<Buffer>,
        cursor_position: language::Anchor,
        debounce: bool,
        cx: &mut Context<Self>,
    ) {
        let copilot = self.copilot.clone();
        self.pending_refresh = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor()
                    .timer(COPILOT_DEBOUNCE_TIMEOUT)
                    .await;
            }

            let completions = copilot
                .update(cx, |copilot, cx| {
                    copilot.completions(&buffer, cursor_position, cx)
                })?
                .await?;

            this.update(cx, |this, cx| {
                if !completions.is_empty() {
                    this.cycled = false;
                    this.pending_refresh = None;
                    this.pending_cycling_refresh = None;
                    this.completions.clear();
                    this.active_completion_index = 0;
                    this.buffer_id = Some(buffer.entity_id());
                    this.file_extension = buffer.read(cx).file().and_then(|file| {
                        Some(
                            Path::new(file.file_name(cx))
                                .extension()?
                                .to_str()?
                                .to_string(),
                        )
                    });

                    for completion in completions {
                        this.push_completion(completion);
                    }
                    cx.notify();
                }
            })?;

            Ok(())
        }));
    }

    fn cycle(
        &mut self,
        buffer: Entity<Buffer>,
        cursor_position: language::Anchor,
        direction: Direction,
        cx: &mut Context<Self>,
    ) {
        if self.cycled {
            match direction {
                Direction::Prev => {
                    self.active_completion_index = if self.active_completion_index == 0 {
                        self.completions.len().saturating_sub(1)
                    } else {
                        self.active_completion_index - 1
                    };
                }
                Direction::Next => {
                    if self.completions.is_empty() {
                        self.active_completion_index = 0
                    } else {
                        self.active_completion_index =
                            (self.active_completion_index + 1) % self.completions.len();
                    }
                }
            }

            cx.notify();
        } else {
            let copilot = self.copilot.clone();
            self.pending_cycling_refresh = Some(cx.spawn(async move |this, cx| {
                let completions = copilot
                    .update(cx, |copilot, cx| {
                        copilot.completions_cycling(&buffer, cursor_position, cx)
                    })?
                    .await?;

                this.update(cx, |this, cx| {
                    this.cycled = true;
                    this.file_extension = buffer.read(cx).file().and_then(|file| {
                        Some(
                            Path::new(file.file_name(cx))
                                .extension()?
                                .to_str()?
                                .to_string(),
                        )
                    });
                    for completion in completions {
                        this.push_completion(completion);
                    }
                    this.cycle(buffer, cursor_position, direction, cx);
                })?;

                Ok(())
            }));
        }
    }

    fn accept(&mut self, cx: &mut Context<Self>) {
        if let Some(completion) = self.active_completion() {
            self.copilot
                .update(cx, |copilot, cx| copilot.accept_completion(completion, cx))
                .detach_and_log_err(cx);
        }
    }

    fn discard(&mut self, cx: &mut Context<Self>) {
        let settings = AllLanguageSettings::get_global(cx);

        let copilot_enabled = settings.show_edit_predictions(None, cx);

        if !copilot_enabled {
            return;
        }

        self.copilot
            .update(cx, |copilot, cx| {
                copilot.discard_completions(&self.completions, cx)
            })
            .detach_and_log_err(cx);
    }

    fn suggest(
        &mut self,
        buffer: &Entity<Buffer>,
        cursor_position: language::Anchor,
        cx: &mut Context<Self>,
    ) -> Option<EditPrediction> {
        let buffer_id = buffer.entity_id();
        let buffer = buffer.read(cx);
        let completion = self.active_completion()?;
        if Some(buffer_id) != self.buffer_id
            || !completion.range.start.is_valid(buffer)
            || !completion.range.end.is_valid(buffer)
        {
            return None;
        }

        let mut completion_range = completion.range.to_offset(buffer);
        let prefix_len = common_prefix(
            buffer.chars_for_range(completion_range.clone()),
            completion.text.chars(),
        );
        completion_range.start += prefix_len;
        let suffix_len = common_prefix(
            buffer.reversed_chars_for_range(completion_range.clone()),
            completion.text[prefix_len..].chars().rev(),
        );
        completion_range.end = completion_range.end.saturating_sub(suffix_len);

        if completion_range.is_empty()
            && completion_range.start == cursor_position.to_offset(buffer)
        {
            let completion_text = &completion.text[prefix_len..completion.text.len() - suffix_len];
            if completion_text.trim().is_empty() {
                None
            } else {
                let position = cursor_position.bias_right(buffer);
                Some(EditPrediction {
                    id: None,
                    edits: vec![(position..position, completion_text.into())],
                    edit_preview: None,
                })
            }
        } else {
            None
        }
    }
}

fn common_prefix<T1: Iterator<Item = char>, T2: Iterator<Item = char>>(a: T1, b: T2) -> usize {
    a.zip(b)
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum()
}
