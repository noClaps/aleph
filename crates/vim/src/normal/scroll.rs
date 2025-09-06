use crate::Vim;
use editor::{
    DisplayPoint, Editor, EditorSettings, SelectionEffects,
    display_map::{DisplayRow, ToDisplayPoint},
    scroll::ScrollAmount,
};
use gpui::{Context, Window, actions};
use language::Bias;
use settings::Settings;
use text::SelectionGoal;

actions!(
    vim,
    [
        /// Scrolls up by one line.
        LineUp,
        /// Scrolls down by one line.
        LineDown,
        /// Scrolls right by one column.
        ColumnRight,
        /// Scrolls left by one column.
        ColumnLeft,
        /// Scrolls up by half a page.
        ScrollUp,
        /// Scrolls down by half a page.
        ScrollDown,
        /// Scrolls up by one page.
        PageUp,
        /// Scrolls down by one page.
        PageDown,
        /// Scrolls right by half a page's width.
        HalfPageRight,
        /// Scrolls left by half a page's width.
        HalfPageLeft,
    ]
);

pub fn register(editor: &mut Editor, cx: &mut Context<Vim>) {
    Vim::action(editor, cx, |vim, _: &LineDown, window, cx| {
        vim.scroll(false, window, cx, |c| ScrollAmount::Line(c.unwrap_or(1.)))
    });
    Vim::action(editor, cx, |vim, _: &LineUp, window, cx| {
        vim.scroll(false, window, cx, |c| ScrollAmount::Line(-c.unwrap_or(1.)))
    });
    Vim::action(editor, cx, |vim, _: &ColumnRight, window, cx| {
        vim.scroll(false, window, cx, |c| ScrollAmount::Column(c.unwrap_or(1.)))
    });
    Vim::action(editor, cx, |vim, _: &ColumnLeft, window, cx| {
        vim.scroll(false, window, cx, |c| {
            ScrollAmount::Column(-c.unwrap_or(1.))
        })
    });
    Vim::action(editor, cx, |vim, _: &PageDown, window, cx| {
        vim.scroll(false, window, cx, |c| ScrollAmount::Page(c.unwrap_or(1.)))
    });
    Vim::action(editor, cx, |vim, _: &PageUp, window, cx| {
        vim.scroll(false, window, cx, |c| ScrollAmount::Page(-c.unwrap_or(1.)))
    });
    Vim::action(editor, cx, |vim, _: &HalfPageRight, window, cx| {
        vim.scroll(false, window, cx, |c| {
            ScrollAmount::PageWidth(c.unwrap_or(0.5))
        })
    });
    Vim::action(editor, cx, |vim, _: &HalfPageLeft, window, cx| {
        vim.scroll(false, window, cx, |c| {
            ScrollAmount::PageWidth(-c.unwrap_or(0.5))
        })
    });
    Vim::action(editor, cx, |vim, _: &ScrollDown, window, cx| {
        vim.scroll(true, window, cx, |c| {
            if let Some(c) = c {
                ScrollAmount::Line(c)
            } else {
                ScrollAmount::Page(0.5)
            }
        })
    });
    Vim::action(editor, cx, |vim, _: &ScrollUp, window, cx| {
        vim.scroll(true, window, cx, |c| {
            if let Some(c) = c {
                ScrollAmount::Line(-c)
            } else {
                ScrollAmount::Page(-0.5)
            }
        })
    });
}

impl Vim {
    fn scroll(
        &mut self,
        move_cursor: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        by: fn(c: Option<f32>) -> ScrollAmount,
    ) {
        let amount = by(Vim::take_count(cx).map(|c| c as f32));
        Vim::take_forced_motion(cx);
        self.exit_temporary_normal(window, cx);
        self.update_editor(cx, |_, editor, cx| {
            scroll_editor(editor, move_cursor, amount, window, cx)
        });
    }
}

fn scroll_editor(
    editor: &mut Editor,
    preserve_cursor_position: bool,
    amount: ScrollAmount,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    let should_move_cursor = editor.newest_selection_on_screen(cx).is_eq();
    let old_top_anchor = editor.scroll_manager.anchor().anchor;

    if editor.scroll_hover(amount, window, cx) {
        return;
    }

    let full_page_up = amount.is_full_page() && amount.direction().is_upwards();
    let amount = match (amount.is_full_page(), editor.visible_line_count()) {
        (true, Some(visible_line_count)) => {
            if amount.direction().is_upwards() {
                ScrollAmount::Line(amount.lines(visible_line_count) + 1.0)
            } else {
                ScrollAmount::Line(amount.lines(visible_line_count) - 1.0)
            }
        }
        _ => amount,
    };

    editor.scroll_screen(&amount, window, cx);
    if !should_move_cursor {
        return;
    }

    let Some(visible_line_count) = editor.visible_line_count() else {
        return;
    };

    let Some(visible_column_count) = editor.visible_column_count() else {
        return;
    };

    let top_anchor = editor.scroll_manager.anchor().anchor;
    let vertical_scroll_margin = EditorSettings::get_global(cx).vertical_scroll_margin;

    editor.change_selections(
        SelectionEffects::no_scroll().nav_history(false),
        window,
        cx,
        |s| {
            s.move_with(|map, selection| {
                // TODO: Improve the logic and function calls below to be dependent on
                // the `amount`. If the amount is vertical, we don't care about
                // columns, while if it's horizontal, we don't care about rows,
                // so we don't need to calculate both and deal with logic for
                // both.
                let mut head = selection.head();
                let top = top_anchor.to_display_point(map);
                let max_point = map.max_point();
                let starting_column = head.column();

                let vertical_scroll_margin =
                    (vertical_scroll_margin as u32).min(visible_line_count as u32 / 2);

                if preserve_cursor_position {
                    let old_top = old_top_anchor.to_display_point(map);
                    let new_row = if old_top.row() == top.row() {
                        DisplayRow(
                            head.row()
                                .0
                                .saturating_add_signed(amount.lines(visible_line_count) as i32),
                        )
                    } else {
                        DisplayRow(top.row().0 + selection.head().row().0 - old_top.row().0)
                    };
                    head = map.clip_point(DisplayPoint::new(new_row, head.column()), Bias::Left)
                }

                let min_row = if top.row().0 == 0 {
                    DisplayRow(0)
                } else {
                    DisplayRow(top.row().0 + vertical_scroll_margin)
                };

                let max_visible_row = top.row().0.saturating_add(
                    (visible_line_count as u32).saturating_sub(1 + vertical_scroll_margin),
                );
                // scroll off the end.
                let max_row = if top.row().0 + visible_line_count as u32 >= max_point.row().0 {
                    max_point.row()
                } else {
                    DisplayRow(
                        (top.row().0 + visible_line_count as u32)
                            .saturating_sub(1 + vertical_scroll_margin),
                    )
                };

                let new_row = if full_page_up {
                    // Special-casing ctrl-b/page-up, which is special-cased by Vim, it seems
                    // to always put the cursor on the last line of the page, even if the cursor
                    // was before that.
                    DisplayRow(max_visible_row)
                } else if head.row() < min_row {
                    min_row
                } else if head.row() > max_row {
                    max_row
                } else {
                    head.row()
                };

                // The minimum column position that the cursor position can be
                // at is either the scroll manager's anchor column, which is the
                // left-most column in the visible area, or the scroll manager's
                // old anchor column, in case the cursor position is being
                // preserved. This is necessary for motions like `ctrl-d` in
                // case there's not enough content to scroll half page down, in
                // which case the scroll manager's anchor column will be the
                // maximum column for the current line, so the minimum column
                // would end up being the same as the maximum column.
                let min_column = match preserve_cursor_position {
                    true => old_top_anchor.to_display_point(map).column(),
                    false => top.column(),
                };

                // As for the maximum column position, that should be either the
                // right-most column in the visible area, which we can easily
                // calculate by adding the visible column count to the minimum
                // column position, or the right-most column in the current
                // line, seeing as the cursor might be in a short line, in which
                // case we don't want to go past its last column.
                let max_row_column = if new_row <= map.max_point().row() {
                    map.line_len(new_row)
                } else {
                    0
                };
                let max_column = match min_column + visible_column_count as u32 {
                    max_column if max_column >= max_row_column => max_row_column,
                    max_column => max_column,
                };

                // Ensure that the cursor's column stays within the visible
                // area, otherwise clip it at either the left or right edge of
                // the visible area.
                let new_column = match (min_column, max_column) {
                    (min_column, _) if starting_column < min_column => min_column,
                    (_, max_column) if starting_column > max_column => max_column,
                    _ => starting_column,
                };

                let new_head = map.clip_point(DisplayPoint::new(new_row, new_column), Bias::Left);
                let goal = match amount {
                    ScrollAmount::Column(_) | ScrollAmount::PageWidth(_) => SelectionGoal::None,
                    _ => selection.goal,
                };

                if selection.is_empty() {
                    selection.collapse_to(new_head, goal)
                } else {
                    selection.set_head(new_head, goal)
                };
            })
        },
    );
}
