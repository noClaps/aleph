use std::fmt;

use gpui::{App, AppContext as _, Entity, EventEmitter, Global, ReadGlobal as _};
use thiserror::Error;

#[derive(Error, Debug)]
pub struct PaymentRequiredError;

impl fmt::Display for PaymentRequiredError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Payment required to use this language model. Please upgrade your account."
        )
    }
}

#[derive(Error, Debug)]
pub struct ToolUseLimitReachedError;

impl fmt::Display for ToolUseLimitReachedError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Consecutive tool use limit reached. Enable Burn Mode for unlimited tool use."
        )
    }
}

#[derive(Clone, Default)]
pub struct LlmApiToken;

struct GlobalRefreshLlmTokenListener(Entity<RefreshLlmTokenListener>);

impl Global for GlobalRefreshLlmTokenListener {}

pub struct RefreshLlmTokenEvent;

pub struct RefreshLlmTokenListener;

impl EventEmitter<RefreshLlmTokenEvent> for RefreshLlmTokenListener {}

impl RefreshLlmTokenListener {
    pub fn register(cx: &mut App) {
        let listener = cx.new(|_cx| RefreshLlmTokenListener::new());
        cx.set_global(GlobalRefreshLlmTokenListener(listener));
    }

    pub fn global(cx: &App) -> Entity<Self> {
        GlobalRefreshLlmTokenListener::global(cx).0.clone()
    }

    fn new() -> Self {
        Self
    }
}
