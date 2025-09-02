use client::UserStore;
use collections::HashMap;
use copilot::{Copilot, CopilotCompletionProvider};
use editor::Editor;
use gpui::{AnyWindowHandle, App, AppContext as _, Context, Entity, WeakEntity};
use language::language_settings::{EditPredictionProvider, all_language_settings};
use settings::SettingsStore;
use std::{cell::RefCell, rc::Rc};
use supermaven::{Supermaven, SupermavenCompletionProvider};
use ui::Window;

pub fn init(user_store: Entity<UserStore>, cx: &mut App) {
    let editors: Rc<RefCell<HashMap<WeakEntity<Editor>, AnyWindowHandle>>> = Rc::default();
    cx.observe_new({
        let editors = editors.clone();
        move |editor: &mut Editor, window, cx: &mut Context<Editor>| {
            if !editor.mode().is_full() {
                return;
            }

            register_backward_compatible_actions(editor, cx);

            let Some(window) = window else {
                return;
            };

            let editor_handle = cx.entity().downgrade();
            cx.on_release({
                let editor_handle = editor_handle.clone();
                let editors = editors.clone();
                move |_, _| {
                    editors.borrow_mut().remove(&editor_handle);
                }
            })
            .detach();

            editors
                .borrow_mut()
                .insert(editor_handle, window.window_handle());
            let provider = all_language_settings(None, cx).edit_predictions.provider;
            assign_edit_prediction_provider(editor, provider, window, cx);
        }
    })
    .detach();

    let mut provider = all_language_settings(None, cx).edit_predictions.provider;
    cx.subscribe(&user_store, {
        let editors = editors.clone();

        move |_, event, cx| {
            if let client::user::Event::PrivateUserInfoUpdated = event {
                assign_edit_prediction_providers(&editors, provider, cx);
            }
        }
    })
    .detach();

    cx.observe_global::<SettingsStore>({
        move |cx| {
            let new_provider = all_language_settings(None, cx).edit_predictions.provider;

            if new_provider != provider {
                telemetry::event!(
                    "Edit Prediction Provider Changed",
                    from = provider,
                    to = new_provider,
                );

                provider = new_provider;
                assign_edit_prediction_providers(&editors, provider, cx);
            }
        }
    })
    .detach();
}

fn assign_edit_prediction_providers(
    editors: &Rc<RefCell<HashMap<WeakEntity<Editor>, AnyWindowHandle>>>,
    provider: EditPredictionProvider,
    cx: &mut App,
) {
    for (editor, window) in editors.borrow().iter() {
        _ = window.update(cx, |_window, window, cx| {
            _ = editor.update(cx, |editor, cx| {
                assign_edit_prediction_provider(editor, provider, window, cx);
            })
        });
    }
}

fn register_backward_compatible_actions(editor: &mut Editor, cx: &mut Context<Editor>) {
    // We renamed some of these actions to not be copilot-specific, but that
    // would have not been backwards-compatible. So here we are re-registering
    // the actions with the old names to not break people's keymaps.
    editor
        .register_action(cx.listener(
            |editor, _: &copilot::Suggest, window: &mut Window, cx: &mut Context<Editor>| {
                editor.show_edit_prediction(&Default::default(), window, cx);
            },
        ))
        .detach();
    editor
        .register_action(cx.listener(
            |editor, _: &copilot::NextSuggestion, window: &mut Window, cx: &mut Context<Editor>| {
                editor.next_edit_prediction(&Default::default(), window, cx);
            },
        ))
        .detach();
    editor
        .register_action(cx.listener(
            |editor,
             _: &copilot::PreviousSuggestion,
             window: &mut Window,
             cx: &mut Context<Editor>| {
                editor.previous_edit_prediction(&Default::default(), window, cx);
            },
        ))
        .detach();
}

fn assign_edit_prediction_provider(
    editor: &mut Editor,
    provider: EditPredictionProvider,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    // TODO: Do we really want to collect data only for singleton buffers?
    let singleton_buffer = editor.buffer().read(cx).as_singleton();

    match provider {
        EditPredictionProvider::Copilot => {
            if let Some(copilot) = Copilot::global(cx) {
                if let Some(buffer) = singleton_buffer
                    && buffer.read(cx).file().is_some()
                {
                    copilot.update(cx, |copilot, cx| {
                        copilot.register_buffer(&buffer, cx);
                    });
                }
                let provider = cx.new(|_| CopilotCompletionProvider::new(copilot));
                editor.set_edit_prediction_provider(Some(provider), window, cx);
            }
        }
        EditPredictionProvider::Supermaven => {
            if let Some(supermaven) = Supermaven::global(cx) {
                let provider = cx.new(|_| SupermavenCompletionProvider::new(supermaven));
                editor.set_edit_prediction_provider(Some(provider), window, cx);
            }
        }
        _ => {}
    }
}
