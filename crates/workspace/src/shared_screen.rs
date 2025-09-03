use client::proto::PeerId;
use gpui::{EventEmitter, FocusHandle, Focusable};
use ui::prelude::*;

pub enum Event {
    Close,
}

pub struct SharedScreen {
    pub peer_id: PeerId,
    focus: FocusHandle,
}

impl SharedScreen {
    pub fn new(peer_id: PeerId, cx: &mut Context<Self>) -> Self {
        Self {
            peer_id,
            focus: cx.focus_handle(),
        }
    }
}

impl EventEmitter<Event> for SharedScreen {}

impl Focusable for SharedScreen {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
