use std::sync::OnceLock;

use regex::Regex;
use smallvec::SmallVec;
use util::debug_panic;

static START_MARKER: OnceLock<Regex> = OnceLock::new();
static END_MARKER: OnceLock<Regex> = OnceLock::new();

#[derive(Debug)]
pub enum CreateFileParserEvent {
    NewTextChunk { chunk: String },
}

#[derive(Debug)]
pub struct CreateFileParser {
    state: ParserState,
    buffer: String,
}

#[derive(Debug, PartialEq)]
enum ParserState {
    Pending,
    WithinText,
    Finishing,
    Finished,
}

impl CreateFileParser {
    pub fn new() -> Self {
        CreateFileParser {
            state: ParserState::Pending,
            buffer: String::new(),
        }
    }

    pub fn push(&mut self, chunk: Option<&str>) -> SmallVec<[CreateFileParserEvent; 1]> {
        if chunk.is_none() {
            self.state = ParserState::Finishing;
        }

        let chunk = chunk.unwrap_or_default();

        self.buffer.push_str(chunk);

        let mut edit_events = SmallVec::new();
        let start_marker_regex = START_MARKER.get_or_init(|| Regex::new(r"\n?```\S*\n").unwrap());
        let end_marker_regex = END_MARKER.get_or_init(|| Regex::new(r"(^|\n)```\s*$").unwrap());
        loop {
            match &mut self.state {
                ParserState::Pending => {
                    if let Some(m) = start_marker_regex.find(&self.buffer) {
                        self.buffer.drain(..m.end());
                        self.state = ParserState::WithinText;
                    } else {
                        break;
                    }
                }
                ParserState::WithinText => {
                    let text = self.buffer.trim_end_matches(&['`', '\n', ' ']);
                    let text_len = text.len();

                    if text_len > 0 {
                        edit_events.push(CreateFileParserEvent::NewTextChunk {
                            chunk: self.buffer.drain(..text_len).collect(),
                        });
                    }
                    break;
                }
                ParserState::Finishing => {
                    if let Some(m) = end_marker_regex.find(&self.buffer) {
                        self.buffer.drain(m.start()..);
                    }
                    if !self.buffer.is_empty() {
                        if !self.buffer.ends_with('\n') {
                            self.buffer.push('\n');
                        }
                        edit_events.push(CreateFileParserEvent::NewTextChunk {
                            chunk: self.buffer.drain(..).collect(),
                        });
                    }
                    self.state = ParserState::Finished;
                    break;
                }
                ParserState::Finished => debug_panic!("Can't call parser after finishing"),
            }
        }
        edit_events
    }
}
