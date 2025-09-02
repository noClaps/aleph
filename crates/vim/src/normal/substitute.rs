use editor::{Editor, SelectionEffects, movement};
use gpui::{Context, Window, actions};
use language::Point;

use crate::{
    Mode, Vim,
    motion::{Motion, MotionKind},
};

actions!(
    vim,
    [
        /// Substitutes characters in the current selection.
        Substitute,
        /// Substitutes the entire line.
        SubstituteLine
    ]
);

pub(crate) fn register(editor: &mut Editor, cx: &mut Context<Vim>) {
    Vim::action(editor, cx, |vim, _: &Substitute, window, cx| {
        vim.start_recording(cx);
        let count = Vim::take_count(cx);
        Vim::take_forced_motion(cx);
        vim.substitute(count, vim.mode == Mode::VisualLine, window, cx);
    });

    Vim::action(editor, cx, |vim, _: &SubstituteLine, window, cx| {
        vim.start_recording(cx);
        if matches!(vim.mode, Mode::VisualBlock | Mode::Visual) {
            vim.switch_mode(Mode::VisualLine, false, window, cx)
        }
        let count = Vim::take_count(cx);
        Vim::take_forced_motion(cx);
        vim.substitute(count, true, window, cx)
    });
}

impl Vim {
    pub fn substitute(
        &mut self,
        count: Option<usize>,
        line_mode: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store_visual_marks(window, cx);
        self.update_editor(cx, |vim, editor, cx| {
            editor.set_clip_at_line_ends(false, cx);
            editor.transact(window, cx, |editor, window, cx| {
                let text_layout_details = editor.text_layout_details(window);
                editor.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                    s.move_with(|map, selection| {
                        if selection.start == selection.end {
                            Motion::Right.expand_selection(
                                map,
                                selection,
                                count,
                                &text_layout_details,
                                false,
                            );
                        }
                        if line_mode {
                            // in Visual mode when the selection contains the newline at the end
                            // of the line, we should exclude it.
                            if !selection.is_empty() && selection.end.column() == 0 {
                                selection.end = movement::left(map, selection.end);
                            }
                            Motion::CurrentLine.expand_selection(
                                map,
                                selection,
                                None,
                                &text_layout_details,
                                false,
                            );
                            if let Some((point, _)) = (Motion::FirstNonWhitespace {
                                display_lines: false,
                            })
                            .move_point(
                                map,
                                selection.start,
                                selection.goal,
                                None,
                                &text_layout_details,
                            ) {
                                selection.start = point;
                            }
                        }
                    })
                });
                let kind = if line_mode {
                    MotionKind::Linewise
                } else {
                    MotionKind::Exclusive
                };
                vim.copy_selections_content(editor, kind, window, cx);
                let selections = editor.selections.all::<Point>(cx).into_iter();
                let edits = selections.map(|selection| (selection.start..selection.end, ""));
                editor.edit(edits, cx);
            });
        });
        self.switch_mode(Mode::Insert, true, window, cx);
    }
}
