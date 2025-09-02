use crate::HandleTag;
use crate::html_element::HtmlElement;
use crate::markdown_writer::{HandlerOutcome, MarkdownWriter, StartTagOutcome};

pub struct WikipediaChromeRemover;

impl HandleTag for WikipediaChromeRemover {
    fn should_handle(&self, _tag: &str) -> bool {
        true
    }

    fn handle_tag_start(
        &mut self,
        tag: &HtmlElement,
        _writer: &mut MarkdownWriter,
    ) -> StartTagOutcome {
        match tag.tag() {
            "head" | "script" | "style" | "nav" => return StartTagOutcome::Skip,
            "sup" => {
                if tag.has_class("reference") {
                    return StartTagOutcome::Skip;
                }
            }
            "div" | "span" | "a" => {
                if tag.attr("id").as_deref() == Some("p-lang-btn") {
                    return StartTagOutcome::Skip;
                }

                if tag.attr("id").as_deref() == Some("p-search") {
                    return StartTagOutcome::Skip;
                }

                let classes_to_skip = ["noprint", "mw-editsection", "mw-jump-link"];
                if tag.has_any_classes(&classes_to_skip) {
                    return StartTagOutcome::Skip;
                }
            }
            _ => {}
        }

        StartTagOutcome::Continue
    }
}

pub struct WikipediaInfoboxHandler;

impl HandleTag for WikipediaInfoboxHandler {
    fn should_handle(&self, tag: &str) -> bool {
        tag == "table"
    }

    fn handle_tag_start(
        &mut self,
        tag: &HtmlElement,
        _writer: &mut MarkdownWriter,
    ) -> StartTagOutcome {
        if tag.tag() == "table" && tag.has_class("infobox") {
            return StartTagOutcome::Skip;
        }

        StartTagOutcome::Continue
    }
}

pub struct WikipediaCodeHandler {
    language: Option<String>,
}

impl WikipediaCodeHandler {
    pub fn new() -> Self {
        Self { language: None }
    }
}

impl Default for WikipediaCodeHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl HandleTag for WikipediaCodeHandler {
    fn should_handle(&self, tag: &str) -> bool {
        matches!(tag, "div" | "pre" | "code")
    }

    fn handle_tag_start(
        &mut self,
        tag: &HtmlElement,
        writer: &mut MarkdownWriter,
    ) -> StartTagOutcome {
        match tag.tag() {
            "code" => {
                if !writer.is_inside("pre") {
                    writer.push_str("`");
                }
            }
            "div" => {
                let classes = tag.classes();
                self.language = classes.iter().find_map(|class| {
                    if let Some((_, language)) = class.split_once("mw-highlight-lang-") {
                        Some(language.trim().to_owned())
                    } else {
                        None
                    }
                });
            }
            "pre" => {
                writer.push_blank_line();
                writer.push_str("```");
                if let Some(language) = self.language.take() {
                    writer.push_str(&language);
                }
                writer.push_newline();
            }
            _ => {}
        }

        StartTagOutcome::Continue
    }

    fn handle_tag_end(&mut self, tag: &HtmlElement, writer: &mut MarkdownWriter) {
        match tag.tag() {
            "code" => {
                if !writer.is_inside("pre") {
                    writer.push_str("`");
                }
            }
            "pre" => writer.push_str("\n```\n"),
            _ => {}
        }
    }

    fn handle_text(&mut self, text: &str, writer: &mut MarkdownWriter) -> HandlerOutcome {
        if writer.is_inside("pre") {
            writer.push_str(text);
            return HandlerOutcome::Handled;
        }

        HandlerOutcome::NoOp
    }
}
