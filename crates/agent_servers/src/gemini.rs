use std::any::Any;
use std::rc::Rc;

use crate::AgentServer;
use gpui::SharedString;

#[derive(Clone)]
pub struct Gemini;

impl AgentServer for Gemini {
    fn telemetry_id(&self) -> &'static str {
        "gemini-cli"
    }

    fn name(&self) -> SharedString {
        "Gemini CLI".into()
    }

    fn logo(&self) -> ui::IconName {
        ui::IconName::AiGemini
    }

    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
}
