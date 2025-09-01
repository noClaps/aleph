use super::RemoteVideoTrack;
use futures::StreamExt as _;
use gpui::{
    AppContext as _, Context, Empty, Entity, EventEmitter, IntoElement, Render, Task, Window,
};

pub struct RemoteVideoTrackView {
    track: RemoteVideoTrack,
    latest_frame: Option<crate::RemoteVideoFrame>,
    _maintain_frame: Task<()>,
}

#[derive(Debug)]
pub enum RemoteVideoTrackViewEvent {
    Close,
}

impl RemoteVideoTrackView {
    pub fn new(track: RemoteVideoTrack, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.focus_handle();
        let frames = crate::play_remote_video_track(&track);

        Self {
            track,
            latest_frame: None,
            _maintain_frame: cx.spawn_in(window, async move |this, cx| {
                futures::pin_mut!(frames);
                while let Some(frame) = frames.next().await {
                    this.update(cx, |this, cx| {
                        this.latest_frame = Some(frame);
                        cx.notify();
                    })
                    .ok();
                }
                this.update(cx, |_this, cx| cx.emit(RemoteVideoTrackViewEvent::Close))
                    .ok();
            }),
        }
    }

    pub fn clone(&self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Self> {
        cx.new(|cx| Self::new(self.track.clone(), window, cx))
    }
}

impl EventEmitter<RemoteVideoTrackViewEvent> for RemoteVideoTrackView {}

impl Render for RemoteVideoTrackView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(latest_frame) = &self.latest_frame {
            use gpui::Styled as _;
            return gpui::surface(latest_frame.clone())
                .size_full()
                .into_any_element();
        }

        Empty.into_any_element()
    }
}
