use crate::{Editor, RangeToAnchorExt};
use gpui::{Context, HighlightStyle, Window};
use language::CursorShape;
use theme::ActiveTheme;

enum MatchingBracketHighlight {}

pub fn refresh_matching_bracket_highlights(
    editor: &mut Editor,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    editor.clear_highlights::<MatchingBracketHighlight>(cx);

    let newest_selection = editor.selections.newest::<usize>(cx);
    // Don't highlight brackets if the selection isn't empty
    if !newest_selection.is_empty() {
        return;
    }

    let snapshot = editor.snapshot(window, cx);
    let head = newest_selection.head();
    if head > snapshot.buffer_snapshot.len() {
        log::error!("bug: cursor offset is out of range while refreshing bracket highlights");
        return;
    }

    let mut tail = head;
    if (editor.cursor_shape == CursorShape::Block || editor.cursor_shape == CursorShape::Hollow)
        && head < snapshot.buffer_snapshot.len()
    {
        tail += 1;
    }

    if let Some((opening_range, closing_range)) = snapshot
        .buffer_snapshot
        .innermost_enclosing_bracket_ranges(head..tail, None)
    {
        editor.highlight_text::<MatchingBracketHighlight>(
            vec![
                opening_range.to_anchors(&snapshot.buffer_snapshot),
                closing_range.to_anchors(&snapshot.buffer_snapshot),
            ],
            HighlightStyle {
                background_color: Some(
                    cx.theme()
                        .colors()
                        .editor_document_highlight_bracket_background,
                ),
                ..Default::default()
            },
            cx,
        )
    }
}
