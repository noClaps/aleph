use anyhow::bail;
use derive_more::{Add, AddAssign};
use language_model::LanguageModel;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::{mem, ops::Range, str::FromStr, sync::Arc};

const OLD_TEXT_END_TAG: &str = "</old_text>";
const NEW_TEXT_END_TAG: &str = "</new_text>";
const EDITS_END_TAG: &str = "</edits>";
const SEARCH_MARKER: &str = "<<<<<<< SEARCH";
const SEPARATOR_MARKER: &str = "=======";
const REPLACE_MARKER: &str = ">>>>>>> REPLACE";
const END_TAGS: [&str; 3] = [OLD_TEXT_END_TAG, NEW_TEXT_END_TAG, EDITS_END_TAG];

#[derive(Debug)]
pub enum EditParserEvent {
    OldTextChunk {
        chunk: String,
        done: bool,
        line_hint: Option<u32>,
    },
    NewTextChunk {
        chunk: String,
        done: bool,
    },
}

#[derive(
    Clone, Debug, Default, PartialEq, Eq, Add, AddAssign, Serialize, Deserialize, JsonSchema,
)]
pub struct EditParserMetrics {
    pub tags: usize,
    pub mismatched_tags: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditFormat {
    /// XML-like tags:
    /// <old_text>...</old_text>
    /// <new_text>...</new_text>
    XmlTags,
    /// Diff-fenced format, in which:
    /// - Text before the SEARCH marker is ignored
    /// - Fences are optional
    /// - Line hint is optional.
    ///
    /// Example:
    ///
    /// ```diff
    /// <<<<<<< SEARCH line=42
    /// ...
    /// =======
    /// ...
    /// >>>>>>> REPLACE
    /// ```
    DiffFenced,
}

impl FromStr for EditFormat {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "xml_tags" | "xml" => Ok(EditFormat::XmlTags),
            "diff_fenced" | "diff-fenced" | "diff" => Ok(EditFormat::DiffFenced),
            _ => bail!("Unknown EditFormat: {}", s),
        }
    }
}

impl EditFormat {
    /// Return an optimal edit format for the language model
    pub fn from_model(model: Arc<dyn LanguageModel>) -> anyhow::Result<Self> {
        if model.provider_id().0 == "google" || model.id().0.to_lowercase().contains("gemini") {
            Ok(EditFormat::DiffFenced)
        } else {
            Ok(EditFormat::XmlTags)
        }
    }

    /// Return an optimal edit format for the language model,
    /// with the ability to override it by setting the
    /// `ZED_EDIT_FORMAT` environment variable
    #[allow(dead_code)]
    pub fn from_env(model: Arc<dyn LanguageModel>) -> anyhow::Result<Self> {
        let default = EditFormat::from_model(model)?;
        std::env::var("ZED_EDIT_FORMAT").map_or(Ok(default), |s| EditFormat::from_str(&s))
    }
}

pub trait EditFormatParser: Send + std::fmt::Debug {
    fn push(&mut self, chunk: &str) -> SmallVec<[EditParserEvent; 1]>;
    fn take_metrics(&mut self) -> EditParserMetrics;
}

#[derive(Debug)]
pub struct XmlEditParser {
    state: XmlParserState,
    buffer: String,
    metrics: EditParserMetrics,
}

#[derive(Debug, PartialEq)]
enum XmlParserState {
    Pending,
    WithinOldText { start: bool, line_hint: Option<u32> },
    AfterOldText,
    WithinNewText { start: bool },
}

#[derive(Debug)]
pub struct DiffFencedEditParser {
    state: DiffParserState,
    buffer: String,
    metrics: EditParserMetrics,
}

#[derive(Debug, PartialEq)]
enum DiffParserState {
    Pending,
    WithinSearch { start: bool, line_hint: Option<u32> },
    WithinReplace { start: bool },
}

/// Main parser that delegates to format-specific parsers
pub struct EditParser {
    parser: Box<dyn EditFormatParser>,
}

impl XmlEditParser {
    pub fn new() -> Self {
        XmlEditParser {
            state: XmlParserState::Pending,
            buffer: String::new(),
            metrics: EditParserMetrics::default(),
        }
    }

    fn find_end_tag(&self) -> Option<Range<usize>> {
        let (tag, start_ix) = END_TAGS
            .iter()
            .flat_map(|tag| Some((tag, self.buffer.find(tag)?)))
            .min_by_key(|(_, ix)| *ix)?;
        Some(start_ix..start_ix + tag.len())
    }

    fn ends_with_tag_prefix(&self) -> bool {
        let mut end_prefixes = END_TAGS
            .iter()
            .flat_map(|tag| (1..tag.len()).map(move |i| &tag[..i]))
            .chain(["\n"]);
        end_prefixes.any(|prefix| self.buffer.ends_with(&prefix))
    }

    fn parse_line_hint(&self, tag: &str) -> Option<u32> {
        use std::sync::LazyLock;
        static LINE_HINT_REGEX: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"line=(?:"?)(\d+)"#).unwrap());

        LINE_HINT_REGEX
            .captures(tag)
            .and_then(|caps| caps.get(1))
            .and_then(|m| m.as_str().parse::<u32>().ok())
    }
}

impl EditFormatParser for XmlEditParser {
    fn push(&mut self, chunk: &str) -> SmallVec<[EditParserEvent; 1]> {
        self.buffer.push_str(chunk);

        let mut edit_events = SmallVec::new();
        loop {
            match &mut self.state {
                XmlParserState::Pending => {
                    if let Some(start) = self.buffer.find("<old_text") {
                        if let Some(tag_end) = self.buffer[start..].find('>') {
                            let tag_end = start + tag_end + 1;
                            let tag = &self.buffer[start..tag_end];
                            let line_hint = self.parse_line_hint(tag);
                            self.buffer.drain(..tag_end);
                            self.state = XmlParserState::WithinOldText {
                                start: true,
                                line_hint,
                            };
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                XmlParserState::WithinOldText { start, line_hint } => {
                    if !self.buffer.is_empty() {
                        if *start && self.buffer.starts_with('\n') {
                            self.buffer.remove(0);
                        }
                        *start = false;
                    }

                    let line_hint = *line_hint;
                    if let Some(tag_range) = self.find_end_tag() {
                        let mut chunk = self.buffer[..tag_range.start].to_string();
                        if chunk.ends_with('\n') {
                            chunk.pop();
                        }

                        self.metrics.tags += 1;
                        if &self.buffer[tag_range.clone()] != OLD_TEXT_END_TAG {
                            self.metrics.mismatched_tags += 1;
                        }

                        self.buffer.drain(..tag_range.end);
                        self.state = XmlParserState::AfterOldText;
                        edit_events.push(EditParserEvent::OldTextChunk {
                            chunk,
                            done: true,
                            line_hint,
                        });
                    } else {
                        if !self.ends_with_tag_prefix() {
                            edit_events.push(EditParserEvent::OldTextChunk {
                                chunk: mem::take(&mut self.buffer),
                                done: false,
                                line_hint,
                            });
                        }
                        break;
                    }
                }
                XmlParserState::AfterOldText => {
                    if let Some(start) = self.buffer.find("<new_text>") {
                        self.buffer.drain(..start + "<new_text>".len());
                        self.state = XmlParserState::WithinNewText { start: true };
                    } else {
                        break;
                    }
                }
                XmlParserState::WithinNewText { start } => {
                    if !self.buffer.is_empty() {
                        if *start && self.buffer.starts_with('\n') {
                            self.buffer.remove(0);
                        }
                        *start = false;
                    }

                    if let Some(tag_range) = self.find_end_tag() {
                        let mut chunk = self.buffer[..tag_range.start].to_string();
                        if chunk.ends_with('\n') {
                            chunk.pop();
                        }

                        self.metrics.tags += 1;
                        if &self.buffer[tag_range.clone()] != NEW_TEXT_END_TAG {
                            self.metrics.mismatched_tags += 1;
                        }

                        self.buffer.drain(..tag_range.end);
                        self.state = XmlParserState::Pending;
                        edit_events.push(EditParserEvent::NewTextChunk { chunk, done: true });
                    } else {
                        if !self.ends_with_tag_prefix() {
                            edit_events.push(EditParserEvent::NewTextChunk {
                                chunk: mem::take(&mut self.buffer),
                                done: false,
                            });
                        }
                        break;
                    }
                }
            }
        }
        edit_events
    }

    fn take_metrics(&mut self) -> EditParserMetrics {
        std::mem::take(&mut self.metrics)
    }
}

impl DiffFencedEditParser {
    pub fn new() -> Self {
        DiffFencedEditParser {
            state: DiffParserState::Pending,
            buffer: String::new(),
            metrics: EditParserMetrics::default(),
        }
    }

    fn ends_with_diff_marker_prefix(&self) -> bool {
        let diff_markers = [SEPARATOR_MARKER, REPLACE_MARKER];
        let mut diff_prefixes = diff_markers
            .iter()
            .flat_map(|marker| (1..marker.len()).map(move |i| &marker[..i]))
            .chain(["\n"]);
        diff_prefixes.any(|prefix| self.buffer.ends_with(&prefix))
    }

    fn parse_line_hint(&self, search_line: &str) -> Option<u32> {
        use regex::Regex;
        use std::sync::LazyLock;
        static LINE_HINT_REGEX: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"line=(?:"?)(\d+)"#).unwrap());

        LINE_HINT_REGEX
            .captures(search_line)
            .and_then(|caps| caps.get(1))
            .and_then(|m| m.as_str().parse::<u32>().ok())
    }
}

impl EditFormatParser for DiffFencedEditParser {
    fn push(&mut self, chunk: &str) -> SmallVec<[EditParserEvent; 1]> {
        self.buffer.push_str(chunk);

        let mut edit_events = SmallVec::new();
        loop {
            match &mut self.state {
                DiffParserState::Pending => {
                    if let Some(diff) = self.buffer.find(SEARCH_MARKER) {
                        let search_end = diff + SEARCH_MARKER.len();
                        if let Some(newline_pos) = self.buffer[search_end..].find('\n') {
                            let search_line = &self.buffer[diff..search_end + newline_pos];
                            let line_hint = self.parse_line_hint(search_line);
                            self.buffer.drain(..search_end + newline_pos + 1);
                            self.state = DiffParserState::WithinSearch {
                                start: true,
                                line_hint,
                            };
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                DiffParserState::WithinSearch { start, line_hint } => {
                    if !self.buffer.is_empty() {
                        if *start && self.buffer.starts_with('\n') {
                            self.buffer.remove(0);
                        }
                        *start = false;
                    }

                    let line_hint = *line_hint;
                    if let Some(separator_pos) = self.buffer.find(SEPARATOR_MARKER) {
                        let mut chunk = self.buffer[..separator_pos].to_string();
                        if chunk.ends_with('\n') {
                            chunk.pop();
                        }

                        let separator_end = separator_pos + SEPARATOR_MARKER.len();
                        if let Some(newline_pos) = self.buffer[separator_end..].find('\n') {
                            self.buffer.drain(..separator_end + newline_pos + 1);
                            self.state = DiffParserState::WithinReplace { start: true };
                            edit_events.push(EditParserEvent::OldTextChunk {
                                chunk,
                                done: true,
                                line_hint,
                            });
                        } else {
                            break;
                        }
                    } else {
                        if !self.ends_with_diff_marker_prefix() {
                            edit_events.push(EditParserEvent::OldTextChunk {
                                chunk: mem::take(&mut self.buffer),
                                done: false,
                                line_hint,
                            });
                        }
                        break;
                    }
                }
                DiffParserState::WithinReplace { start } => {
                    if !self.buffer.is_empty() {
                        if *start && self.buffer.starts_with('\n') {
                            self.buffer.remove(0);
                        }
                        *start = false;
                    }

                    if let Some(replace_pos) = self.buffer.find(REPLACE_MARKER) {
                        let mut chunk = self.buffer[..replace_pos].to_string();
                        if chunk.ends_with('\n') {
                            chunk.pop();
                        }

                        self.buffer.drain(..replace_pos + REPLACE_MARKER.len());
                        if let Some(newline_pos) = self.buffer.find('\n') {
                            self.buffer.drain(..newline_pos + 1);
                        } else {
                            self.buffer.clear();
                        }

                        self.state = DiffParserState::Pending;
                        edit_events.push(EditParserEvent::NewTextChunk { chunk, done: true });
                    } else {
                        if !self.ends_with_diff_marker_prefix() {
                            edit_events.push(EditParserEvent::NewTextChunk {
                                chunk: mem::take(&mut self.buffer),
                                done: false,
                            });
                        }
                        break;
                    }
                }
            }
        }
        edit_events
    }

    fn take_metrics(&mut self) -> EditParserMetrics {
        std::mem::take(&mut self.metrics)
    }
}

impl EditParser {
    pub fn new(format: EditFormat) -> Self {
        let parser: Box<dyn EditFormatParser> = match format {
            EditFormat::XmlTags => Box::new(XmlEditParser::new()),
            EditFormat::DiffFenced => Box::new(DiffFencedEditParser::new()),
        };
        EditParser { parser }
    }

    pub fn push(&mut self, chunk: &str) -> SmallVec<[EditParserEvent; 1]> {
        self.parser.push(chunk)
    }

    pub fn finish(mut self) -> EditParserMetrics {
        self.parser.take_metrics()
    }
}
