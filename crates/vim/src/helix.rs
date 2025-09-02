use editor::display_map::DisplaySnapshot;
use editor::{
    DisplayPoint, Editor, HideMouseCursorOrigin, SelectionEffects, ToOffset, ToPoint, movement,
};
use gpui::{Action, actions};
use gpui::{Context, Window};
use language::{CharClassifier, CharKind, Point};
use text::{Bias, SelectionGoal};

use crate::motion;
use crate::{
    Vim,
    motion::{Motion, right},
    state::Mode,
};

actions!(
    vim,
    [
        /// Switches to normal mode after the cursor (Helix-style).
        HelixNormalAfter,
        /// Yanks the current selection or character if no selection.
        HelixYank,
        /// Inserts at the beginning of the selection.
        HelixInsert,
        /// Appends at the end of the selection.
        HelixAppend,
        /// Goes to the location of the last modification.
        HelixGotoLastModification,
        /// Select entire line or multiple lines, extending downwards.
        HelixSelectLine,
    ]
);

pub fn register(editor: &mut Editor, cx: &mut Context<Vim>) {
    Vim::action(editor, cx, Vim::helix_normal_after);
    Vim::action(editor, cx, Vim::helix_select_lines);
    Vim::action(editor, cx, Vim::helix_insert);
    Vim::action(editor, cx, Vim::helix_append);
    Vim::action(editor, cx, Vim::helix_yank);
    Vim::action(editor, cx, Vim::helix_goto_last_modification);
}

impl Vim {
    pub fn helix_normal_after(
        &mut self,
        action: &HelixNormalAfter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_operator().is_some() {
            self.operator_stack.clear();
            self.sync_vim_settings(window, cx);
            return;
        }
        self.stop_recording_immediately(action.boxed_clone(), cx);
        self.switch_mode(Mode::HelixNormal, false, window, cx);
    }

    pub fn helix_normal_motion(
        &mut self,
        motion: Motion,
        times: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.helix_move_cursor(motion, times, window, cx);
    }

    /// Updates all selections based on where the cursors are.
    fn helix_new_selections(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        mut change: impl FnMut(
            // the start of the cursor
            DisplayPoint,
            &DisplaySnapshot,
        ) -> Option<(DisplayPoint, DisplayPoint)>,
    ) {
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_with(|map, selection| {
                    let cursor_start = if selection.reversed || selection.is_empty() {
                        selection.head()
                    } else {
                        movement::left(map, selection.head())
                    };
                    let Some((head, tail)) = change(cursor_start, map) else {
                        return;
                    };

                    selection.set_head_tail(head, tail, SelectionGoal::None);
                });
            });
        });
    }

    fn helix_find_range_forward(
        &mut self,
        times: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
        mut is_boundary: impl FnMut(char, char, &CharClassifier) -> bool,
    ) {
        let times = times.unwrap_or(1);
        self.helix_new_selections(window, cx, |cursor, map| {
            let mut head = movement::right(map, cursor);
            let mut tail = cursor;
            let classifier = map.buffer_snapshot.char_classifier_at(head.to_point(map));
            if head == map.max_point() {
                return None;
            }
            for _ in 0..times {
                let (maybe_next_tail, next_head) =
                    movement::find_boundary_trail(map, head, |left, right| {
                        is_boundary(left, right, &classifier)
                    });

                if next_head == head && maybe_next_tail.unwrap_or(next_head) == tail {
                    break;
                }

                head = next_head;
                if let Some(next_tail) = maybe_next_tail {
                    tail = next_tail;
                }
            }
            Some((head, tail))
        });
    }

    fn helix_find_range_backward(
        &mut self,
        times: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
        mut is_boundary: impl FnMut(char, char, &CharClassifier) -> bool,
    ) {
        let times = times.unwrap_or(1);
        self.helix_new_selections(window, cx, |cursor, map| {
            let mut head = cursor;
            // The original cursor was one character wide,
            // but the search starts from the left side of it,
            // so to include that space the selection must end one character to the right.
            let mut tail = movement::right(map, cursor);
            let classifier = map.buffer_snapshot.char_classifier_at(head.to_point(map));
            if head == DisplayPoint::zero() {
                return None;
            }
            for _ in 0..times {
                let (maybe_next_tail, next_head) =
                    movement::find_preceding_boundary_trail(map, head, |left, right| {
                        is_boundary(left, right, &classifier)
                    });

                if next_head == head && maybe_next_tail.unwrap_or(next_head) == tail {
                    break;
                }

                head = next_head;
                if let Some(next_tail) = maybe_next_tail {
                    tail = next_tail;
                }
            }
            Some((head, tail))
        });
    }

    pub fn helix_move_and_collapse(
        &mut self,
        motion: Motion,
        times: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_editor(cx, |_, editor, cx| {
            let text_layout_details = editor.text_layout_details(window);
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_with(|map, selection| {
                    let goal = selection.goal;
                    let cursor = if selection.is_empty() || selection.reversed {
                        selection.head()
                    } else {
                        movement::left(map, selection.head())
                    };

                    let (point, goal) = motion
                        .move_point(map, cursor, selection.goal, times, &text_layout_details)
                        .unwrap_or((cursor, goal));

                    selection.collapse_to(point, goal)
                })
            });
        });
    }

    pub fn helix_move_cursor(
        &mut self,
        motion: Motion,
        times: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match motion {
            Motion::NextWordStart { ignore_punctuation } => {
                self.helix_find_range_forward(times, window, cx, |left, right, classifier| {
                    let left_kind = classifier.kind_with(left, ignore_punctuation);
                    let right_kind = classifier.kind_with(right, ignore_punctuation);
                    let at_newline = (left == '\n') ^ (right == '\n');

                    (left_kind != right_kind && right_kind != CharKind::Whitespace) || at_newline
                })
            }
            Motion::NextWordEnd { ignore_punctuation } => {
                self.helix_find_range_forward(times, window, cx, |left, right, classifier| {
                    let left_kind = classifier.kind_with(left, ignore_punctuation);
                    let right_kind = classifier.kind_with(right, ignore_punctuation);
                    let at_newline = (left == '\n') ^ (right == '\n');

                    (left_kind != right_kind && left_kind != CharKind::Whitespace) || at_newline
                })
            }
            Motion::PreviousWordStart { ignore_punctuation } => {
                self.helix_find_range_backward(times, window, cx, |left, right, classifier| {
                    let left_kind = classifier.kind_with(left, ignore_punctuation);
                    let right_kind = classifier.kind_with(right, ignore_punctuation);
                    let at_newline = (left == '\n') ^ (right == '\n');

                    (left_kind != right_kind && left_kind != CharKind::Whitespace) || at_newline
                })
            }
            Motion::PreviousWordEnd { ignore_punctuation } => {
                self.helix_find_range_backward(times, window, cx, |left, right, classifier| {
                    let left_kind = classifier.kind_with(left, ignore_punctuation);
                    let right_kind = classifier.kind_with(right, ignore_punctuation);
                    let at_newline = (left == '\n') ^ (right == '\n');

                    (left_kind != right_kind && right_kind != CharKind::Whitespace) || at_newline
                })
            }
            Motion::FindForward {
                before,
                char,
                mode,
                smartcase,
            } => {
                self.helix_new_selections(window, cx, |cursor, map| {
                    let start = cursor;
                    let mut last_boundary = start;
                    for _ in 0..times.unwrap_or(1) {
                        last_boundary = movement::find_boundary(
                            map,
                            movement::right(map, last_boundary),
                            mode,
                            |left, right| {
                                let current_char = if before { right } else { left };
                                motion::is_character_match(char, current_char, smartcase)
                            },
                        );
                    }
                    Some((last_boundary, start))
                });
            }
            Motion::FindBackward {
                after,
                char,
                mode,
                smartcase,
            } => {
                self.helix_new_selections(window, cx, |cursor, map| {
                    let start = cursor;
                    let mut last_boundary = start;
                    for _ in 0..times.unwrap_or(1) {
                        last_boundary = movement::find_preceding_boundary_display_point(
                            map,
                            last_boundary,
                            mode,
                            |left, right| {
                                let current_char = if after { left } else { right };
                                motion::is_character_match(char, current_char, smartcase)
                            },
                        );
                    }
                    // The original cursor was one character wide,
                    // but the search started from the left side of it,
                    // so to include that space the selection must end one character to the right.
                    Some((last_boundary, movement::right(map, start)))
                });
            }
            _ => self.helix_move_and_collapse(motion, times, window, cx),
        }
    }

    pub fn helix_yank(&mut self, _: &HelixYank, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |vim, editor, cx| {
            let has_selection = editor
                .selections
                .all_adjusted(cx)
                .iter()
                .any(|selection| !selection.is_empty());

            if !has_selection {
                // If no selection, expand to current character (like 'v' does)
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.move_with(|map, selection| {
                        let head = selection.head();
                        let new_head = movement::saturating_right(map, head);
                        selection.set_tail(head, SelectionGoal::None);
                        selection.set_head(new_head, SelectionGoal::None);
                    });
                });
                vim.yank_selections_content(
                    editor,
                    crate::motion::MotionKind::Exclusive,
                    window,
                    cx,
                );
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.move_with(|_map, selection| {
                        selection.collapse_to(selection.start, SelectionGoal::None);
                    });
                });
            } else {
                // Yank the selection(s)
                vim.yank_selections_content(
                    editor,
                    crate::motion::MotionKind::Exclusive,
                    window,
                    cx,
                );
            }
        });
    }

    fn helix_insert(&mut self, _: &HelixInsert, window: &mut Window, cx: &mut Context<Self>) {
        self.start_recording(cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_with(|_map, selection| {
                    // In helix normal mode, move cursor to start of selection and collapse
                    if !selection.is_empty() {
                        selection.collapse_to(selection.start, SelectionGoal::None);
                    }
                });
            });
        });
        self.switch_mode(Mode::Insert, false, window, cx);
    }

    fn helix_append(&mut self, _: &HelixAppend, window: &mut Window, cx: &mut Context<Self>) {
        self.start_recording(cx);
        self.switch_mode(Mode::Insert, false, window, cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_with(|map, selection| {
                    let point = if selection.is_empty() {
                        right(map, selection.head(), 1)
                    } else {
                        selection.end
                    };
                    selection.collapse_to(point, SelectionGoal::None);
                });
            });
        });
    }

    pub fn helix_replace(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            editor.transact(window, cx, |editor, window, cx| {
                let (map, selections) = editor.selections.all_display(cx);

                // Store selection info for positioning after edit
                let selection_info: Vec<_> = selections
                    .iter()
                    .map(|selection| {
                        let range = selection.range();
                        let start_offset = range.start.to_offset(&map, Bias::Left);
                        let end_offset = range.end.to_offset(&map, Bias::Left);
                        let was_empty = range.is_empty();
                        let was_reversed = selection.reversed;
                        (
                            map.buffer_snapshot.anchor_at(start_offset, Bias::Left),
                            end_offset - start_offset,
                            was_empty,
                            was_reversed,
                        )
                    })
                    .collect();

                let mut edits = Vec::new();
                for selection in &selections {
                    let mut range = selection.range();

                    // For empty selections, extend to replace one character
                    if range.is_empty() {
                        range.end = movement::saturating_right(&map, range.start);
                    }

                    let byte_range = range.start.to_offset(&map, Bias::Left)
                        ..range.end.to_offset(&map, Bias::Left);

                    if !byte_range.is_empty() {
                        let replacement_text = text.repeat(byte_range.len());
                        edits.push((byte_range, replacement_text));
                    }
                }

                editor.edit(edits, cx);

                // Restore selections based on original info
                let snapshot = editor.buffer().read(cx).snapshot(cx);
                let ranges: Vec<_> = selection_info
                    .into_iter()
                    .map(|(start_anchor, original_len, was_empty, was_reversed)| {
                        let start_point = start_anchor.to_point(&snapshot);
                        if was_empty {
                            // For cursor-only, collapse to start
                            start_point..start_point
                        } else {
                            // For selections, span the replaced text
                            let replacement_len = text.len() * original_len;
                            let end_offset = start_anchor.to_offset(&snapshot) + replacement_len;
                            let end_point = snapshot.offset_to_point(end_offset);
                            if was_reversed {
                                end_point..start_point
                            } else {
                                start_point..end_point
                            }
                        }
                    })
                    .collect();

                editor.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                    s.select_ranges(ranges);
                });
            });
        });
        self.switch_mode(Mode::HelixNormal, true, window, cx);
    }

    pub fn helix_goto_last_modification(
        &mut self,
        _: &HelixGotoLastModification,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.jump(".".into(), false, false, window, cx);
    }

    pub fn helix_select_lines(
        &mut self,
        _: &HelixSelectLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = Vim::take_count(cx).unwrap_or(1);
        self.update_editor(cx, |_, editor, cx| {
            editor.hide_mouse_cursor(HideMouseCursorOrigin::MovementAction, cx);
            let display_map = editor.display_map.update(cx, |map, cx| map.snapshot(cx));
            let mut selections = editor.selections.all::<Point>(cx);
            let max_point = display_map.buffer_snapshot.max_point();
            let buffer_snapshot = &display_map.buffer_snapshot;

            for selection in &mut selections {
                // Start always goes to column 0 of the first selected line
                let start_row = selection.start.row;
                let current_end_row = selection.end.row;

                // Check if cursor is on empty line by checking first character
                let line_start_offset = buffer_snapshot.point_to_offset(Point::new(start_row, 0));
                let first_char = buffer_snapshot.chars_at(line_start_offset).next();
                let extra_line = if first_char == Some('\n') { 1 } else { 0 };

                let end_row = current_end_row + count as u32 + extra_line;

                selection.start = Point::new(start_row, 0);
                selection.end = if end_row > max_point.row {
                    max_point
                } else {
                    Point::new(end_row, 0)
                };
                selection.reversed = false;
            }

            editor.change_selections(Default::default(), window, cx, |s| {
                s.select(selections);
            });
        });
    }
}
