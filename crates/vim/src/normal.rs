mod change;
mod convert;
mod delete;
mod increment;
pub(crate) mod mark;
mod paste;
pub(crate) mod repeat;
mod scroll;
pub(crate) mod search;
pub mod substitute;
mod toggle_comments;
pub(crate) mod yank;

use std::collections::HashMap;
use std::sync::Arc;

use crate::{
    Vim,
    indent::IndentDirection,
    motion::{self, Motion, first_non_whitespace, next_line_end, right},
    object::Object,
    state::{Mark, Mode, Operator},
    surrounds::SurroundsType,
};
use collections::BTreeSet;
use convert::ConvertTarget;
use editor::Editor;
use editor::{Anchor, SelectionEffects};
use editor::{Bias, ToPoint};
use editor::{display_map::ToDisplayPoint, movement};
use gpui::{Context, Window, actions};
use language::{Point, SelectionGoal};
use log::error;
use multi_buffer::MultiBufferRow;

actions!(
    vim,
    [
        /// Inserts text after the cursor.
        InsertAfter,
        /// Inserts text before the cursor.
        InsertBefore,
        /// Inserts at the first non-whitespace character.
        InsertFirstNonWhitespace,
        /// Inserts at the end of the line.
        InsertEndOfLine,
        /// Inserts a new line above the current line.
        InsertLineAbove,
        /// Inserts a new line below the current line.
        InsertLineBelow,
        /// Inserts an empty line above without entering insert mode.
        InsertEmptyLineAbove,
        /// Inserts an empty line below without entering insert mode.
        InsertEmptyLineBelow,
        /// Inserts at the previous insert position.
        InsertAtPrevious,
        /// Joins the current line with the next line.
        JoinLines,
        /// Joins lines without adding whitespace.
        JoinLinesNoWhitespace,
        /// Deletes character to the left.
        DeleteLeft,
        /// Deletes character to the right.
        DeleteRight,
        /// Deletes using Helix-style behavior.
        HelixDelete,
        /// Collapse the current selection
        HelixCollapseSelection,
        /// Changes from cursor to end of line.
        ChangeToEndOfLine,
        /// Deletes from cursor to end of line.
        DeleteToEndOfLine,
        /// Yanks (copies) the selected text.
        Yank,
        /// Yanks the entire line.
        YankLine,
        /// Toggles the case of selected text.
        ChangeCase,
        /// Converts selected text to uppercase.
        ConvertToUpperCase,
        /// Converts selected text to lowercase.
        ConvertToLowerCase,
        /// Applies ROT13 cipher to selected text.
        ConvertToRot13,
        /// Applies ROT47 cipher to selected text.
        ConvertToRot47,
        /// Toggles comments for selected lines.
        ToggleComments,
        /// Shows the current location in the file.
        ShowLocation,
        /// Undoes the last change.
        Undo,
        /// Redoes the last undone change.
        Redo,
        /// Undoes all changes to the most recently changed line.
        UndoLastLine,
    ]
);

pub(crate) fn register(editor: &mut Editor, cx: &mut Context<Vim>) {
    Vim::action(editor, cx, Vim::insert_after);
    Vim::action(editor, cx, Vim::insert_before);
    Vim::action(editor, cx, Vim::insert_first_non_whitespace);
    Vim::action(editor, cx, Vim::insert_end_of_line);
    Vim::action(editor, cx, Vim::insert_line_above);
    Vim::action(editor, cx, Vim::insert_line_below);
    Vim::action(editor, cx, Vim::insert_empty_line_above);
    Vim::action(editor, cx, Vim::insert_empty_line_below);
    Vim::action(editor, cx, Vim::insert_at_previous);
    Vim::action(editor, cx, Vim::change_case);
    Vim::action(editor, cx, Vim::convert_to_upper_case);
    Vim::action(editor, cx, Vim::convert_to_lower_case);
    Vim::action(editor, cx, Vim::convert_to_rot13);
    Vim::action(editor, cx, Vim::convert_to_rot47);
    Vim::action(editor, cx, Vim::yank_line);
    Vim::action(editor, cx, Vim::toggle_comments);
    Vim::action(editor, cx, Vim::paste);
    Vim::action(editor, cx, Vim::show_location);

    Vim::action(editor, cx, |vim, _: &DeleteLeft, window, cx| {
        vim.record_current_action(cx);
        let times = Vim::take_count(cx);
        let forced_motion = Vim::take_forced_motion(cx);
        vim.delete_motion(Motion::Left, times, forced_motion, window, cx);
    });
    Vim::action(editor, cx, |vim, _: &DeleteRight, window, cx| {
        vim.record_current_action(cx);
        let times = Vim::take_count(cx);
        let forced_motion = Vim::take_forced_motion(cx);
        vim.delete_motion(Motion::Right, times, forced_motion, window, cx);
    });

    Vim::action(editor, cx, |vim, _: &HelixDelete, window, cx| {
        vim.record_current_action(cx);
        vim.update_editor(cx, |_, editor, cx| {
            editor.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                s.move_with(|map, selection| {
                    if selection.is_empty() {
                        selection.end = movement::right(map, selection.end)
                    }
                })
            })
        });
        vim.visual_delete(false, window, cx);
        vim.switch_mode(Mode::HelixNormal, true, window, cx);
    });

    Vim::action(editor, cx, |vim, _: &HelixCollapseSelection, window, cx| {
        vim.update_editor(cx, |_, editor, cx| {
            editor.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                s.move_with(|map, selection| {
                    let mut point = selection.head();
                    if !selection.reversed && !selection.is_empty() {
                        point = movement::left(map, selection.head());
                    }
                    selection.collapse_to(point, selection.goal)
                });
            });
        });
    });

    Vim::action(editor, cx, |vim, _: &ChangeToEndOfLine, window, cx| {
        vim.start_recording(cx);
        let times = Vim::take_count(cx);
        let forced_motion = Vim::take_forced_motion(cx);
        vim.change_motion(
            Motion::EndOfLine {
                display_lines: false,
            },
            times,
            forced_motion,
            window,
            cx,
        );
    });
    Vim::action(editor, cx, |vim, _: &DeleteToEndOfLine, window, cx| {
        vim.record_current_action(cx);
        let times = Vim::take_count(cx);
        let forced_motion = Vim::take_forced_motion(cx);
        vim.delete_motion(
            Motion::EndOfLine {
                display_lines: false,
            },
            times,
            forced_motion,
            window,
            cx,
        );
    });
    Vim::action(editor, cx, |vim, _: &JoinLines, window, cx| {
        vim.join_lines_impl(true, window, cx);
    });

    Vim::action(editor, cx, |vim, _: &JoinLinesNoWhitespace, window, cx| {
        vim.join_lines_impl(false, window, cx);
    });

    Vim::action(editor, cx, |vim, _: &Undo, window, cx| {
        let times = Vim::take_count(cx);
        Vim::take_forced_motion(cx);
        vim.update_editor(cx, |_, editor, cx| {
            for _ in 0..times.unwrap_or(1) {
                editor.undo(&editor::actions::Undo, window, cx);
            }
        });
    });
    Vim::action(editor, cx, |vim, _: &Redo, window, cx| {
        let times = Vim::take_count(cx);
        Vim::take_forced_motion(cx);
        vim.update_editor(cx, |_, editor, cx| {
            for _ in 0..times.unwrap_or(1) {
                editor.redo(&editor::actions::Redo, window, cx);
            }
        });
    });
    Vim::action(editor, cx, |vim, _: &UndoLastLine, window, cx| {
        Vim::take_forced_motion(cx);
        vim.update_editor(cx, |vim, editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let Some(last_change) = editor.change_list.last_before_grouping() else {
                return;
            };

            let anchors = last_change.to_vec();
            let mut last_row = None;
            let ranges: Vec<_> = anchors
                .iter()
                .filter_map(|anchor| {
                    let point = anchor.to_point(&snapshot);
                    if last_row == Some(point.row) {
                        return None;
                    }
                    last_row = Some(point.row);
                    let line_range = Point::new(point.row, 0)
                        ..Point::new(point.row, snapshot.line_len(MultiBufferRow(point.row)));
                    Some((
                        snapshot.anchor_before(line_range.start)
                            ..snapshot.anchor_after(line_range.end),
                        line_range,
                    ))
                })
                .collect();

            let edits = editor.buffer().update(cx, |buffer, cx| {
                let current_content = ranges
                    .iter()
                    .map(|(anchors, _)| {
                        buffer
                            .snapshot(cx)
                            .text_for_range(anchors.clone())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>();
                let mut content_before_undo = current_content.clone();
                let mut undo_count = 0;

                loop {
                    let undone_tx = buffer.undo(cx);
                    undo_count += 1;
                    let mut content_after_undo = Vec::new();

                    let mut line_changed = false;
                    for ((anchors, _), text_before_undo) in
                        ranges.iter().zip(content_before_undo.iter())
                    {
                        let snapshot = buffer.snapshot(cx);
                        let text_after_undo =
                            snapshot.text_for_range(anchors.clone()).collect::<String>();

                        if &text_after_undo != text_before_undo {
                            line_changed = true;
                        }
                        content_after_undo.push(text_after_undo);
                    }

                    content_before_undo = content_after_undo;
                    if !line_changed {
                        break;
                    }
                    if undone_tx == vim.undo_last_line_tx {
                        break;
                    }
                }

                let edits = ranges
                    .into_iter()
                    .zip(content_before_undo.into_iter().zip(current_content))
                    .filter_map(|((_, mut points), (mut old_text, new_text))| {
                        if new_text == old_text {
                            return None;
                        }
                        let common_suffix_starts_at = old_text
                            .char_indices()
                            .rev()
                            .zip(new_text.chars().rev())
                            .find_map(
                                |((i, a), b)| {
                                    if a != b { Some(i + a.len_utf8()) } else { None }
                                },
                            )
                            .unwrap_or(old_text.len());
                        points.end.column -= (old_text.len() - common_suffix_starts_at) as u32;
                        old_text = old_text.split_at(common_suffix_starts_at).0.to_string();
                        let common_prefix_len = old_text
                            .char_indices()
                            .zip(new_text.chars())
                            .find_map(|((i, a), b)| if a != b { Some(i) } else { None })
                            .unwrap_or(0);
                        points.start.column = common_prefix_len as u32;
                        old_text = old_text.split_at(common_prefix_len).1.to_string();

                        Some((points, old_text))
                    })
                    .collect::<Vec<_>>();

                for _ in 0..undo_count {
                    buffer.redo(cx);
                }
                edits
            });
            vim.undo_last_line_tx = editor.transact(window, cx, |editor, window, cx| {
                editor.change_list.invert_last_group();
                editor.edit(edits, cx);
                editor.change_selections(SelectionEffects::default(), window, cx, |s| {
                    s.select_anchor_ranges(anchors.into_iter().map(|a| a..a));
                })
            });
        });
    });

    repeat::register(editor, cx);
    scroll::register(editor, cx);
    search::register(editor, cx);
    substitute::register(editor, cx);
    increment::register(editor, cx);
}

impl Vim {
    pub fn normal_motion(
        &mut self,
        motion: Motion,
        operator: Option<Operator>,
        times: Option<usize>,
        forced_motion: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match operator {
            None => self.move_cursor(motion, times, window, cx),
            Some(Operator::Change) => self.change_motion(motion, times, forced_motion, window, cx),
            Some(Operator::Delete) => self.delete_motion(motion, times, forced_motion, window, cx),
            Some(Operator::Yank) => self.yank_motion(motion, times, forced_motion, window, cx),
            Some(Operator::AddSurrounds { target: None }) => {}
            Some(Operator::Indent) => self.indent_motion(
                motion,
                times,
                forced_motion,
                IndentDirection::In,
                window,
                cx,
            ),
            Some(Operator::Rewrap) => self.rewrap_motion(motion, times, forced_motion, window, cx),
            Some(Operator::Outdent) => self.indent_motion(
                motion,
                times,
                forced_motion,
                IndentDirection::Out,
                window,
                cx,
            ),
            Some(Operator::AutoIndent) => self.indent_motion(
                motion,
                times,
                forced_motion,
                IndentDirection::Auto,
                window,
                cx,
            ),
            Some(Operator::ShellCommand) => {
                self.shell_command_motion(motion, times, forced_motion, window, cx)
            }
            Some(Operator::Lowercase) => self.convert_motion(
                motion,
                times,
                forced_motion,
                ConvertTarget::LowerCase,
                window,
                cx,
            ),
            Some(Operator::Uppercase) => self.convert_motion(
                motion,
                times,
                forced_motion,
                ConvertTarget::UpperCase,
                window,
                cx,
            ),
            Some(Operator::OppositeCase) => self.convert_motion(
                motion,
                times,
                forced_motion,
                ConvertTarget::OppositeCase,
                window,
                cx,
            ),
            Some(Operator::Rot13) => self.convert_motion(
                motion,
                times,
                forced_motion,
                ConvertTarget::Rot13,
                window,
                cx,
            ),
            Some(Operator::Rot47) => self.convert_motion(
                motion,
                times,
                forced_motion,
                ConvertTarget::Rot47,
                window,
                cx,
            ),
            Some(Operator::ToggleComments) => {
                self.toggle_comments_motion(motion, times, forced_motion, window, cx)
            }
            Some(Operator::ReplaceWithRegister) => {
                self.replace_with_register_motion(motion, times, forced_motion, window, cx)
            }
            Some(Operator::Exchange) => {
                self.exchange_motion(motion, times, forced_motion, window, cx)
            }
            Some(operator) => {
                // Can't do anything for text objects, Ignoring
                error!("Unexpected normal mode motion operator: {:?}", operator)
            }
        }
        // Exit temporary normal mode (if active).
        self.exit_temporary_normal(window, cx);
    }

    pub fn normal_object(
        &mut self,
        object: Object,
        times: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut waiting_operator: Option<Operator> = None;
        match self.maybe_pop_operator() {
            Some(Operator::Object { around }) => match self.maybe_pop_operator() {
                Some(Operator::Change) => self.change_object(object, around, times, window, cx),
                Some(Operator::Delete) => self.delete_object(object, around, times, window, cx),
                Some(Operator::Yank) => self.yank_object(object, around, times, window, cx),
                Some(Operator::Indent) => {
                    self.indent_object(object, around, IndentDirection::In, times, window, cx)
                }
                Some(Operator::Outdent) => {
                    self.indent_object(object, around, IndentDirection::Out, times, window, cx)
                }
                Some(Operator::AutoIndent) => {
                    self.indent_object(object, around, IndentDirection::Auto, times, window, cx)
                }
                Some(Operator::ShellCommand) => {
                    self.shell_command_object(object, around, window, cx);
                }
                Some(Operator::Rewrap) => self.rewrap_object(object, around, times, window, cx),
                Some(Operator::Lowercase) => {
                    self.convert_object(object, around, ConvertTarget::LowerCase, times, window, cx)
                }
                Some(Operator::Uppercase) => {
                    self.convert_object(object, around, ConvertTarget::UpperCase, times, window, cx)
                }
                Some(Operator::OppositeCase) => self.convert_object(
                    object,
                    around,
                    ConvertTarget::OppositeCase,
                    times,
                    window,
                    cx,
                ),
                Some(Operator::Rot13) => {
                    self.convert_object(object, around, ConvertTarget::Rot13, times, window, cx)
                }
                Some(Operator::Rot47) => {
                    self.convert_object(object, around, ConvertTarget::Rot47, times, window, cx)
                }
                Some(Operator::AddSurrounds { target: None }) => {
                    waiting_operator = Some(Operator::AddSurrounds {
                        target: Some(SurroundsType::Object(object, around)),
                    });
                }
                Some(Operator::ToggleComments) => {
                    self.toggle_comments_object(object, around, times, window, cx)
                }
                Some(Operator::ReplaceWithRegister) => {
                    self.replace_with_register_object(object, around, window, cx)
                }
                Some(Operator::Exchange) => self.exchange_object(object, around, window, cx),
                _ => {
                    // Can't do anything for namespace operators. Ignoring
                }
            },
            Some(Operator::DeleteSurrounds) => {
                waiting_operator = Some(Operator::DeleteSurrounds);
            }
            Some(Operator::ChangeSurrounds { target: None }) => {
                if self.check_and_move_to_valid_bracket_pair(object, window, cx) {
                    waiting_operator = Some(Operator::ChangeSurrounds {
                        target: Some(object),
                    });
                }
            }
            _ => {
                // Can't do anything with change/delete/yank/surrounds and text objects. Ignoring
            }
        }
        self.clear_operator(cx);
        if let Some(operator) = waiting_operator {
            self.push_operator(operator, cx);
        }
    }

    pub(crate) fn move_cursor(
        &mut self,
        motion: Motion,
        times: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_editor(cx, |_, editor, cx| {
            let text_layout_details = editor.text_layout_details(window);
            editor.change_selections(
                SelectionEffects::default().nav_history(motion.push_to_jump_list()),
                window,
                cx,
                |s| {
                    s.move_cursors_with(|map, cursor, goal| {
                        motion
                            .move_point(map, cursor, goal, times, &text_layout_details)
                            .unwrap_or((cursor, goal))
                    })
                },
            )
        });
    }

    fn insert_after(&mut self, _: &InsertAfter, window: &mut Window, cx: &mut Context<Self>) {
        self.start_recording(cx);
        self.switch_mode(Mode::Insert, false, window, cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_cursors_with(|map, cursor, _| (right(map, cursor, 1), SelectionGoal::None));
            });
        });
    }

    fn insert_before(&mut self, _: &InsertBefore, window: &mut Window, cx: &mut Context<Self>) {
        self.start_recording(cx);
        if self.mode.is_visual() {
            let current_mode = self.mode;
            self.update_editor(cx, |_, editor, cx| {
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.move_with(|map, selection| {
                        if current_mode == Mode::VisualLine {
                            let start_of_line = motion::start_of_line(map, false, selection.start);
                            selection.collapse_to(start_of_line, SelectionGoal::None)
                        } else {
                            selection.collapse_to(selection.start, SelectionGoal::None)
                        }
                    });
                });
            });
        }
        self.switch_mode(Mode::Insert, false, window, cx);
    }

    fn insert_first_non_whitespace(
        &mut self,
        _: &InsertFirstNonWhitespace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_recording(cx);
        self.switch_mode(Mode::Insert, false, window, cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_cursors_with(|map, cursor, _| {
                    (
                        first_non_whitespace(map, false, cursor),
                        SelectionGoal::None,
                    )
                });
            });
        });
    }

    fn insert_end_of_line(
        &mut self,
        _: &InsertEndOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_recording(cx);
        self.switch_mode(Mode::Insert, false, window, cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_cursors_with(|map, cursor, _| {
                    (next_line_end(map, cursor, 1), SelectionGoal::None)
                });
            });
        });
    }

    fn insert_at_previous(
        &mut self,
        _: &InsertAtPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_recording(cx);
        self.switch_mode(Mode::Insert, false, window, cx);
        self.update_editor(cx, |vim, editor, cx| {
            let Some(Mark::Local(marks)) = vim.get_mark("^", editor, window, cx) else {
                return;
            };

            editor.change_selections(Default::default(), window, cx, |s| {
                s.select_anchor_ranges(marks.iter().map(|mark| *mark..*mark))
            });
        });
    }

    fn insert_line_above(
        &mut self,
        _: &InsertLineAbove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_recording(cx);
        self.switch_mode(Mode::Insert, false, window, cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.transact(window, cx, |editor, window, cx| {
                let selections = editor.selections.all::<Point>(cx);
                let snapshot = editor.buffer().read(cx).snapshot(cx);

                let selection_start_rows: BTreeSet<u32> = selections
                    .into_iter()
                    .map(|selection| selection.start.row)
                    .collect();
                let edits = selection_start_rows
                    .into_iter()
                    .map(|row| {
                        let indent = snapshot
                            .indent_and_comment_for_line(MultiBufferRow(row), cx)
                            .chars()
                            .collect::<String>();

                        let start_of_line = Point::new(row, 0);
                        (start_of_line..start_of_line, indent + "\n")
                    })
                    .collect::<Vec<_>>();
                editor.edit_with_autoindent(edits, cx);
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.move_cursors_with(|map, cursor, _| {
                        let previous_line = motion::start_of_relative_buffer_row(map, cursor, -1);
                        let insert_point = motion::end_of_line(map, false, previous_line, 1);
                        (insert_point, SelectionGoal::None)
                    });
                });
            });
        });
    }

    fn insert_line_below(
        &mut self,
        _: &InsertLineBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_recording(cx);
        self.switch_mode(Mode::Insert, false, window, cx);
        self.update_editor(cx, |_, editor, cx| {
            let text_layout_details = editor.text_layout_details(window);
            editor.transact(window, cx, |editor, window, cx| {
                let selections = editor.selections.all::<Point>(cx);
                let snapshot = editor.buffer().read(cx).snapshot(cx);

                let selection_end_rows: BTreeSet<u32> = selections
                    .into_iter()
                    .map(|selection| selection.end.row)
                    .collect();
                let edits = selection_end_rows
                    .into_iter()
                    .map(|row| {
                        let indent = snapshot
                            .indent_and_comment_for_line(MultiBufferRow(row), cx)
                            .chars()
                            .collect::<String>();

                        let end_of_line = Point::new(row, snapshot.line_len(MultiBufferRow(row)));
                        (end_of_line..end_of_line, "\n".to_string() + &indent)
                    })
                    .collect::<Vec<_>>();
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.maybe_move_cursors_with(|map, cursor, goal| {
                        Motion::CurrentLine.move_point(
                            map,
                            cursor,
                            goal,
                            None,
                            &text_layout_details,
                        )
                    });
                });
                editor.edit_with_autoindent(edits, cx);
            });
        });
    }

    fn insert_empty_line_above(
        &mut self,
        _: &InsertEmptyLineAbove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.record_current_action(cx);
        let count = Vim::take_count(cx).unwrap_or(1);
        Vim::take_forced_motion(cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.transact(window, cx, |editor, _, cx| {
                let selections = editor.selections.all::<Point>(cx);

                let selection_start_rows: BTreeSet<u32> = selections
                    .into_iter()
                    .map(|selection| selection.start.row)
                    .collect();
                let edits = selection_start_rows
                    .into_iter()
                    .map(|row| {
                        let start_of_line = Point::new(row, 0);
                        (start_of_line..start_of_line, "\n".repeat(count))
                    })
                    .collect::<Vec<_>>();
                editor.edit(edits, cx);
            });
        });
    }

    fn insert_empty_line_below(
        &mut self,
        _: &InsertEmptyLineBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.record_current_action(cx);
        let count = Vim::take_count(cx).unwrap_or(1);
        Vim::take_forced_motion(cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.transact(window, cx, |editor, window, cx| {
                let selections = editor.selections.all::<Point>(cx);
                let snapshot = editor.buffer().read(cx).snapshot(cx);
                let (_map, display_selections) = editor.selections.all_display(cx);
                let original_positions = display_selections
                    .iter()
                    .map(|s| (s.id, s.head()))
                    .collect::<HashMap<_, _>>();

                let selection_end_rows: BTreeSet<u32> = selections
                    .into_iter()
                    .map(|selection| selection.end.row)
                    .collect();
                let edits = selection_end_rows
                    .into_iter()
                    .map(|row| {
                        let end_of_line = Point::new(row, snapshot.line_len(MultiBufferRow(row)));
                        (end_of_line..end_of_line, "\n".repeat(count))
                    })
                    .collect::<Vec<_>>();
                editor.edit(edits, cx);

                editor.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                    s.move_with(|_, selection| {
                        if let Some(position) = original_positions.get(&selection.id) {
                            selection.collapse_to(*position, SelectionGoal::None);
                        }
                    });
                });
            });
        });
    }

    fn join_lines_impl(
        &mut self,
        insert_whitespace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.record_current_action(cx);
        let mut times = Vim::take_count(cx).unwrap_or(1);
        Vim::take_forced_motion(cx);
        if self.mode.is_visual() {
            times = 1;
        } else if times > 1 {
            // 2J joins two lines together (same as J or 1J)
            times -= 1;
        }

        self.update_editor(cx, |_, editor, cx| {
            editor.transact(window, cx, |editor, window, cx| {
                for _ in 0..times {
                    editor.join_lines_impl(insert_whitespace, window, cx)
                }
            })
        });
        if self.mode.is_visual() {
            self.switch_mode(Mode::Normal, true, window, cx)
        }
    }

    fn yank_line(&mut self, _: &YankLine, window: &mut Window, cx: &mut Context<Self>) {
        let count = Vim::take_count(cx);
        let forced_motion = Vim::take_forced_motion(cx);
        self.yank_motion(
            motion::Motion::CurrentLine,
            count,
            forced_motion,
            window,
            cx,
        )
    }

    fn show_location(&mut self, _: &ShowLocation, _: &mut Window, cx: &mut Context<Self>) {
        let count = Vim::take_count(cx);
        Vim::take_forced_motion(cx);
        self.update_editor(cx, |vim, editor, cx| {
            let selection = editor.selections.newest_anchor();
            let Some((buffer, point, _)) = editor
                .buffer()
                .read(cx)
                .point_to_buffer_point(selection.head(), cx)
            else {
                return;
            };
            let filename = if let Some(file) = buffer.read(cx).file() {
                if count.is_some() {
                    if let Some(local) = file.as_local() {
                        local.abs_path(cx).to_string_lossy().to_string()
                    } else {
                        file.full_path(cx).to_string_lossy().to_string()
                    }
                } else {
                    file.path().to_string_lossy().to_string()
                }
            } else {
                "[No Name]".into()
            };
            let buffer = buffer.read(cx);
            let lines = buffer.max_point().row + 1;
            let current_line = point.row;
            let percentage = current_line as f32 / lines as f32;
            let modified = if buffer.is_dirty() { " [modified]" } else { "" };
            vim.status_label = Some(
                format!(
                    "{}{} {} lines --{:.0}%--",
                    filename,
                    modified,
                    lines,
                    percentage * 100.0,
                )
                .into(),
            );
            cx.notify();
        });
    }

    fn toggle_comments(&mut self, _: &ToggleComments, window: &mut Window, cx: &mut Context<Self>) {
        self.record_current_action(cx);
        self.store_visual_marks(window, cx);
        self.update_editor(cx, |vim, editor, cx| {
            editor.transact(window, cx, |editor, window, cx| {
                let original_positions = vim.save_selection_starts(editor, cx);
                editor.toggle_comments(&Default::default(), window, cx);
                vim.restore_selection_cursors(editor, window, cx, original_positions);
            });
        });
        if self.mode.is_visual() {
            self.switch_mode(Mode::Normal, true, window, cx)
        }
    }

    pub(crate) fn normal_replace(
        &mut self,
        text: Arc<str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_return_char = text == "\n".into() || text == "\r".into();
        let count = Vim::take_count(cx).unwrap_or(1);
        Vim::take_forced_motion(cx);
        self.stop_recording(cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.transact(window, cx, |editor, window, cx| {
                editor.set_clip_at_line_ends(false, cx);
                let (map, display_selections) = editor.selections.all_display(cx);

                let mut edits = Vec::new();
                for selection in &display_selections {
                    let mut range = selection.range();
                    for _ in 0..count {
                        let new_point = movement::saturating_right(&map, range.end);
                        if range.end == new_point {
                            return;
                        }
                        range.end = new_point;
                    }

                    edits.push((
                        range.start.to_offset(&map, Bias::Left)
                            ..range.end.to_offset(&map, Bias::Left),
                        text.repeat(if is_return_char { 0 } else { count }),
                    ));
                }

                editor.edit(edits, cx);
                if is_return_char {
                    editor.newline(&editor::actions::Newline, window, cx);
                }
                editor.set_clip_at_line_ends(true, cx);
                editor.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                    s.move_with(|map, selection| {
                        let point = movement::saturating_left(map, selection.head());
                        selection.collapse_to(point, SelectionGoal::None)
                    });
                });
            });
        });
        self.pop_operator(cx);
    }

    pub fn save_selection_starts(
        &self,
        editor: &Editor,

        cx: &mut Context<Editor>,
    ) -> HashMap<usize, Anchor> {
        let (map, selections) = editor.selections.all_display(cx);
        selections
            .iter()
            .map(|selection| {
                (
                    selection.id,
                    map.display_point_to_anchor(selection.start, Bias::Right),
                )
            })
            .collect::<HashMap<_, _>>()
    }

    pub fn restore_selection_cursors(
        &self,
        editor: &mut Editor,
        window: &mut Window,
        cx: &mut Context<Editor>,
        mut positions: HashMap<usize, Anchor>,
    ) {
        editor.change_selections(Default::default(), window, cx, |s| {
            s.move_with(|map, selection| {
                if let Some(anchor) = positions.remove(&selection.id) {
                    selection.collapse_to(anchor.to_display_point(map), SelectionGoal::None);
                }
            });
        });
    }

    fn exit_temporary_normal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.temp_mode {
            self.switch_mode(Mode::Insert, true, window, cx);
        }
    }
}
