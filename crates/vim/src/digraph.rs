use std::sync::Arc;

use collections::HashMap;
use editor::Editor;
use gpui::{Action, App, Context, Keystroke, KeystrokeEvent, Window};
use schemars::JsonSchema;
use serde::Deserialize;
use settings::Settings;
use std::sync::LazyLock;

use crate::{Vim, VimSettings, state::Operator};

mod default;

#[derive(Debug, Clone, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = vim)]
struct Literal(String, char);

pub(crate) fn register(editor: &mut Editor, cx: &mut Context<Vim>) {
    Vim::action(editor, cx, Vim::literal)
}

static DEFAULT_DIGRAPHS_MAP: LazyLock<HashMap<String, Arc<str>>> = LazyLock::new(|| {
    let mut map = HashMap::default();
    for &(a, b, c) in default::DEFAULT_DIGRAPHS {
        let key = format!("{a}{b}");
        let value = char::from_u32(c).unwrap().to_string().into();
        map.insert(key, value);
    }
    map
});

fn lookup_digraph(a: char, b: char, cx: &App) -> Arc<str> {
    let custom_digraphs = &VimSettings::get_global(cx).custom_digraphs;
    let input = format!("{a}{b}");
    let reversed = format!("{b}{a}");

    custom_digraphs
        .get(&input)
        .or_else(|| DEFAULT_DIGRAPHS_MAP.get(&input))
        .or_else(|| custom_digraphs.get(&reversed))
        .or_else(|| DEFAULT_DIGRAPHS_MAP.get(&reversed))
        .cloned()
        .unwrap_or_else(|| b.to_string().into())
}

impl Vim {
    pub fn insert_digraph(
        &mut self,
        first_char: char,
        second_char: char,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = lookup_digraph(first_char, second_char, cx);

        self.pop_operator(window, cx);
        if self.editor_input_enabled() {
            self.update_editor(cx, |_, editor, cx| editor.insert(&text, window, cx));
        } else {
            self.input_ignored(text, window, cx);
        }
    }

    fn literal(&mut self, action: &Literal, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Operator::Literal { prefix }) = self.active_operator()
            && let Some(prefix) = prefix
        {
            if let Some(keystroke) = Keystroke::parse(&action.0).ok() {
                window.defer(cx, |window, cx| {
                    window.dispatch_keystroke(keystroke, cx);
                });
            }
            return self.handle_literal_input(prefix, "", window, cx);
        }

        self.insert_literal(Some(action.1), "", window, cx);
    }

    pub fn handle_literal_keystroke(
        &mut self,
        keystroke_event: &KeystrokeEvent,
        prefix: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // handled by handle_literal_input
        if keystroke_event.keystroke.key_char.is_some() {
            return;
        };

        if !prefix.is_empty() {
            self.handle_literal_input(prefix, "", window, cx);
        } else {
            self.pop_operator(window, cx);
        }

        // give another chance to handle the binding outside
        // of waiting mode.
        if keystroke_event.action.is_none() {
            let keystroke = keystroke_event.keystroke.clone();
            window.defer(cx, |window, cx| {
                window.dispatch_keystroke(keystroke, cx);
            });
        }
    }

    pub fn handle_literal_input(
        &mut self,
        mut prefix: String,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let first = prefix.chars().next();
        let next = text.chars().next().unwrap_or(' ');
        match first {
            Some('o' | 'O') => {
                if next.is_digit(8) {
                    prefix.push(next);
                    if prefix.len() == 4 {
                        let ch: char = u8::from_str_radix(&prefix[1..], 8).unwrap_or(255).into();
                        return self.insert_literal(Some(ch), "", window, cx);
                    }
                } else {
                    let ch = if prefix.len() > 1 {
                        Some(u8::from_str_radix(&prefix[1..], 8).unwrap_or(255).into())
                    } else {
                        None
                    };
                    return self.insert_literal(ch, text, window, cx);
                }
            }
            Some('x' | 'X' | 'u' | 'U') => {
                let max_len = match first.unwrap() {
                    'x' => 3,
                    'X' => 3,
                    'u' => 5,
                    'U' => 9,
                    _ => unreachable!(),
                };
                if next.is_ascii_hexdigit() {
                    prefix.push(next);
                    if prefix.len() == max_len {
                        let ch: char = u32::from_str_radix(&prefix[1..], 16)
                            .ok()
                            .and_then(|n| n.try_into().ok())
                            .unwrap_or('\u{FFFD}');
                        return self.insert_literal(Some(ch), "", window, cx);
                    }
                } else {
                    let ch = if prefix.len() > 1 {
                        Some(
                            u32::from_str_radix(&prefix[1..], 16)
                                .ok()
                                .and_then(|n| n.try_into().ok())
                                .unwrap_or('\u{FFFD}'),
                        )
                    } else {
                        None
                    };
                    return self.insert_literal(ch, text, window, cx);
                }
            }
            Some('0'..='9') => {
                if next.is_ascii_hexdigit() {
                    prefix.push(next);
                    if prefix.len() == 3 {
                        let ch: char = u8::from_str_radix(&prefix, 10).unwrap_or(255).into();
                        return self.insert_literal(Some(ch), "", window, cx);
                    }
                } else {
                    let ch: char = u8::from_str_radix(&prefix, 10).unwrap_or(255).into();
                    return self.insert_literal(Some(ch), "", window, cx);
                }
            }
            None if matches!(next, 'o' | 'O' | 'x' | 'X' | 'u' | 'U' | '0'..='9') => {
                prefix.push(next)
            }
            _ => {
                return self.insert_literal(None, text, window, cx);
            }
        };

        self.pop_operator(window, cx);
        self.push_operator(
            Operator::Literal {
                prefix: Some(prefix),
            },
            window,
            cx,
        );
    }

    fn insert_literal(
        &mut self,
        ch: Option<char>,
        suffix: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pop_operator(window, cx);
        let mut text = String::new();
        if let Some(c) = ch {
            if c == '\n' {
                text.push('\x00')
            } else {
                text.push(c)
            }
        }
        text.push_str(suffix);

        if self.editor_input_enabled() {
            self.update_editor(cx, |_, editor, cx| editor.insert(&text, window, cx));
        } else {
            self.input_ignored(text.into(), window, cx);
        }
    }
}
