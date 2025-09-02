//! TextDiffView currently provides a UI for displaying differences between the clipboard and selected text.

use anyhow::Result;
use buffer_diff::{BufferDiff, BufferDiffSnapshot};
use editor::{Editor, EditorEvent, MultiBuffer, ToPoint, actions::DiffClipboardWithSelectionData};
use futures::{FutureExt, select_biased};
use gpui::{
    AnyElement, AnyView, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter,
    FocusHandle, Focusable, IntoElement, Render, Task, Window,
};
use language::{self, Buffer, Point};
use project::Project;
use std::{
    any::{Any, TypeId},
    cmp,
    ops::Range,
    pin::pin,
    sync::Arc,
    time::Duration,
};
use ui::{Color, Icon, IconName, Label, LabelCommon as _, SharedString};
use util::paths::PathExt;

use workspace::{
    Item, ItemHandle as _, ItemNavHistory, ToolbarItemLocation, Workspace,
    item::{BreadcrumbText, ItemEvent, SaveOptions, TabContentParams},
    searchable::SearchableItemHandle,
};

pub struct TextDiffView {
    diff_editor: Entity<Editor>,
    title: SharedString,
    path: Option<SharedString>,
    buffer_changes_tx: watch::Sender<()>,
    _recalculate_diff_task: Task<Result<()>>,
}

const RECALCULATE_DIFF_DEBOUNCE: Duration = Duration::from_millis(250);

impl TextDiffView {
    pub fn open(
        diff_data: &DiffClipboardWithSelectionData,
        workspace: &Workspace,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Task<Result<Entity<Self>>>> {
        let source_editor = diff_data.editor.clone();

        let selection_data = source_editor.update(cx, |editor, cx| {
            let multibuffer = editor.buffer().read(cx);
            let source_buffer = multibuffer.as_singleton()?;
            let selections = editor.selections.all::<Point>(cx);
            let buffer_snapshot = source_buffer.read(cx);
            let first_selection = selections.first()?;
            let max_point = buffer_snapshot.max_point();

            if first_selection.is_empty() {
                let full_range = Point::new(0, 0)..max_point;
                return Some((source_buffer, full_range));
            }

            let start = first_selection.start;
            let end = first_selection.end;
            let expanded_start = Point::new(start.row, 0);

            let expanded_end = if end.column > 0 {
                let next_row = end.row + 1;
                cmp::min(max_point, Point::new(next_row, 0))
            } else {
                end
            };
            Some((source_buffer, expanded_start..expanded_end))
        });

        let Some((source_buffer, expanded_selection_range)) = selection_data else {
            log::warn!("There should always be at least one selection in Zed. This is a bug.");
            return None;
        };

        source_editor.update(cx, |source_editor, cx| {
            source_editor.change_selections(Default::default(), window, cx, |s| {
                s.select_ranges(vec![
                    expanded_selection_range.start..expanded_selection_range.end,
                ]);
            })
        });

        let source_buffer_snapshot = source_buffer.read(cx).snapshot();
        let mut clipboard_text = diff_data.clipboard_text.clone();

        if !clipboard_text.ends_with("\n") {
            clipboard_text.push_str("\n");
        }

        let workspace = workspace.weak_handle();
        let diff_buffer = cx.new(|cx| BufferDiff::new(&source_buffer_snapshot.text, cx));
        let clipboard_buffer = build_clipboard_buffer(
            clipboard_text,
            &source_buffer,
            expanded_selection_range.clone(),
            cx,
        );

        let task = window.spawn(cx, async move |cx| {
            let project = workspace.update(cx, |workspace, _| workspace.project().clone())?;

            update_diff_buffer(&diff_buffer, &source_buffer, &clipboard_buffer, cx).await?;

            workspace.update_in(cx, |workspace, window, cx| {
                let diff_view = cx.new(|cx| {
                    TextDiffView::new(
                        clipboard_buffer,
                        source_editor,
                        source_buffer,
                        expanded_selection_range,
                        diff_buffer,
                        project,
                        window,
                        cx,
                    )
                });

                let pane = workspace.active_pane();
                pane.update(cx, |pane, cx| {
                    pane.add_item(Box::new(diff_view.clone()), true, true, None, window, cx);
                });

                diff_view
            })
        });

        Some(task)
    }

    pub fn new(
        clipboard_buffer: Entity<Buffer>,
        source_editor: Entity<Editor>,
        source_buffer: Entity<Buffer>,
        source_range: Range<Point>,
        diff_buffer: Entity<BufferDiff>,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let multibuffer = cx.new(|cx| {
            let mut multibuffer = MultiBuffer::new(language::Capability::ReadWrite);

            multibuffer.push_excerpts(
                source_buffer.clone(),
                [editor::ExcerptRange::new(source_range)],
                cx,
            );

            multibuffer.add_diff(diff_buffer.clone(), cx);
            multibuffer
        });
        let diff_editor = cx.new(|cx| {
            let mut editor = Editor::for_multibuffer(multibuffer, Some(project), window, cx);
            editor.start_temporary_diff_override();
            editor.disable_diagnostics(cx);
            editor.set_expand_all_diff_hunks(cx);
            editor.set_render_diff_hunk_controls(
                Arc::new(|_, _, _, _, _, _, _, _| gpui::Empty.into_any_element()),
                cx,
            );
            editor
        });

        let (buffer_changes_tx, mut buffer_changes_rx) = watch::channel(());

        cx.subscribe(&source_buffer, move |this, _, event, _| match event {
            language::BufferEvent::Edited
            | language::BufferEvent::LanguageChanged
            | language::BufferEvent::Reparsed => {
                this.buffer_changes_tx.send(()).ok();
            }
            _ => {}
        })
        .detach();

        let editor = source_editor.read(cx);
        let title = editor.buffer().read(cx).title(cx).to_string();
        let selection_location_text = selection_location_text(editor, cx);
        let selection_location_title = selection_location_text
            .as_ref()
            .map(|text| format!("{} @ {}", title, text))
            .unwrap_or(title);

        let path = editor
            .buffer()
            .read(cx)
            .as_singleton()
            .and_then(|b| {
                b.read(cx)
                    .file()
                    .map(|f| f.full_path(cx).compact().to_string_lossy().to_string())
            })
            .unwrap_or("untitled".into());

        let selection_location_path = selection_location_text
            .map(|text| format!("{} @ {}", path, text))
            .unwrap_or(path);

        Self {
            diff_editor,
            title: format!("Clipboard ↔ {selection_location_title}").into(),
            path: Some(format!("Clipboard ↔ {selection_location_path}").into()),
            buffer_changes_tx,
            _recalculate_diff_task: cx.spawn(async move |_, cx| {
                while buffer_changes_rx.recv().await.is_ok() {
                    loop {
                        let mut timer = cx
                            .background_executor()
                            .timer(RECALCULATE_DIFF_DEBOUNCE)
                            .fuse();
                        let mut recv = pin!(buffer_changes_rx.recv().fuse());
                        select_biased! {
                            _ = timer => break,
                            _ = recv => continue,
                        }
                    }

                    log::trace!("start recalculating");
                    update_diff_buffer(&diff_buffer, &source_buffer, &clipboard_buffer, cx).await?;
                    log::trace!("finish recalculating");
                }
                Ok(())
            }),
        }
    }
}

fn build_clipboard_buffer(
    text: String,
    source_buffer: &Entity<Buffer>,
    replacement_range: Range<Point>,
    cx: &mut App,
) -> Entity<Buffer> {
    let source_buffer_snapshot = source_buffer.read(cx).snapshot();
    cx.new(|cx| {
        let mut buffer = language::Buffer::local(source_buffer_snapshot.text(), cx);
        let language = source_buffer.read(cx).language().cloned();
        buffer.set_language(language, cx);

        let range_start = source_buffer_snapshot.point_to_offset(replacement_range.start);
        let range_end = source_buffer_snapshot.point_to_offset(replacement_range.end);
        buffer.edit([(range_start..range_end, text)], None, cx);

        buffer
    })
}

async fn update_diff_buffer(
    diff: &Entity<BufferDiff>,
    source_buffer: &Entity<Buffer>,
    clipboard_buffer: &Entity<Buffer>,
    cx: &mut AsyncApp,
) -> Result<()> {
    let source_buffer_snapshot = source_buffer.read_with(cx, |buffer, _| buffer.snapshot())?;

    let base_buffer_snapshot = clipboard_buffer.read_with(cx, |buffer, _| buffer.snapshot())?;
    let base_text = base_buffer_snapshot.text();

    let diff_snapshot = cx
        .update(|cx| {
            BufferDiffSnapshot::new_with_base_buffer(
                source_buffer_snapshot.text.clone(),
                Some(Arc::new(base_text)),
                base_buffer_snapshot,
                cx,
            )
        })?
        .await;

    diff.update(cx, |diff, cx| {
        diff.set_snapshot(diff_snapshot, &source_buffer_snapshot.text, cx);
    })?;
    Ok(())
}

impl EventEmitter<EditorEvent> for TextDiffView {}

impl Focusable for TextDiffView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.diff_editor.focus_handle(cx)
    }
}

impl Item for TextDiffView {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Diff).color(Color::Muted))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(params.detail.unwrap_or_default(), cx))
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, _: &App) -> SharedString {
        self.title.clone()
    }

    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        self.path.clone()
    }

    fn to_item_events(event: &EditorEvent, f: impl FnMut(ItemEvent)) {
        Editor::to_item_events(event, f)
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Selection Diff View Opened")
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.diff_editor
            .update(cx, |editor, cx| editor.deactivated(window, cx));
    }

    fn is_singleton(&self, _: &App) -> bool {
        false
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        _: &'a App,
    ) -> Option<AnyView> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.to_any())
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.diff_editor.to_any())
        } else {
            None
        }
    }

    fn as_searchable(&self, _: &Entity<Self>) -> Option<Box<dyn SearchableItemHandle>> {
        Some(Box::new(self.diff_editor.clone()))
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        self.diff_editor.for_each_project_item(cx, f)
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_editor.update(cx, |editor, _| {
            editor.set_nav_history(Some(nav_history));
        });
    }

    fn navigate(
        &mut self,
        data: Box<dyn Any>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.diff_editor
            .update(cx, |editor, cx| editor.navigate(data, window, cx))
    }

    fn breadcrumb_location(&self, _: &App) -> ToolbarItemLocation {
        ToolbarItemLocation::PrimaryLeft
    }

    fn breadcrumbs(&self, theme: &theme::Theme, cx: &App) -> Option<Vec<BreadcrumbText>> {
        self.diff_editor.breadcrumbs(theme, cx)
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_editor.update(cx, |editor, cx| {
            editor.added_to_workspace(workspace, window, cx)
        });
    }

    fn can_save(&self, cx: &App) -> bool {
        // The editor handles the new buffer, so delegate to it
        self.diff_editor.read(cx).can_save(cx)
    }

    fn save(
        &mut self,
        options: SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        // Delegate saving to the editor, which manages the new buffer
        self.diff_editor
            .update(cx, |editor, cx| editor.save(options, project, window, cx))
    }
}

pub fn selection_location_text(editor: &Editor, cx: &App) -> Option<String> {
    let buffer = editor.buffer().read(cx);
    let buffer_snapshot = buffer.snapshot(cx);
    let first_selection = editor.selections.disjoint.first()?;

    let selection_start = first_selection.start.to_point(&buffer_snapshot);
    let selection_end = first_selection.end.to_point(&buffer_snapshot);

    let start_row = selection_start.row;
    let start_column = selection_start.column;
    let end_row = selection_end.row;
    let end_column = selection_end.column;

    let range_text = if start_row == end_row {
        format!("L{}:{}-{}", start_row + 1, start_column + 1, end_column + 1)
    } else {
        format!(
            "L{}:{}-L{}:{}",
            start_row + 1,
            start_column + 1,
            end_row + 1,
            end_column + 1
        )
    };

    Some(range_text)
}

impl Render for TextDiffView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.diff_editor.clone()
    }
}
