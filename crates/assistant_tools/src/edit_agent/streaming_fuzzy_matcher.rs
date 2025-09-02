use language::{Point, TextBufferSnapshot};
use std::{cmp, ops::Range};

const REPLACEMENT_COST: u32 = 1;
const INSERTION_COST: u32 = 3;
const DELETION_COST: u32 = 10;

/// A streaming fuzzy matcher that can process text chunks incrementally
/// and return the best match found so far at each step.
pub struct StreamingFuzzyMatcher {
    snapshot: TextBufferSnapshot,
    query_lines: Vec<String>,
    line_hint: Option<u32>,
    incomplete_line: String,
    matches: Vec<Range<usize>>,
    matrix: SearchMatrix,
}

impl StreamingFuzzyMatcher {
    pub fn new(snapshot: TextBufferSnapshot) -> Self {
        let buffer_line_count = snapshot.max_point().row as usize + 1;
        Self {
            snapshot,
            query_lines: Vec::new(),
            line_hint: None,
            incomplete_line: String::new(),
            matches: Vec::new(),
            matrix: SearchMatrix::new(buffer_line_count + 1),
        }
    }

    /// Returns the query lines.
    pub fn query_lines(&self) -> &[String] {
        &self.query_lines
    }

    /// Push a new chunk of text and get the best match found so far.
    ///
    /// This method accumulates text chunks and processes complete lines.
    /// Partial lines are buffered internally until a newline is received.
    ///
    /// # Returns
    ///
    /// Returns `Some(range)` if a match has been found with the accumulated
    /// query so far, or `None` if no suitable match exists yet.
    pub fn push(&mut self, chunk: &str, line_hint: Option<u32>) -> Option<Range<usize>> {
        if line_hint.is_some() {
            self.line_hint = line_hint;
        }

        // Add the chunk to our incomplete line buffer
        self.incomplete_line.push_str(chunk);
        self.line_hint = line_hint;

        if let Some((last_pos, _)) = self.incomplete_line.match_indices('\n').next_back() {
            let complete_part = &self.incomplete_line[..=last_pos];

            // Split into lines and add to query_lines
            for line in complete_part.lines() {
                self.query_lines.push(line.to_string());
            }

            self.incomplete_line.replace_range(..last_pos + 1, "");

            self.matches = self.resolve_location_fuzzy();
        }

        let best_match = self.select_best_match();
        best_match.or_else(|| self.matches.first().cloned())
    }

    /// Finish processing and return the final best match(es).
    ///
    /// This processes any remaining incomplete line before returning the final
    /// match result.
    pub fn finish(&mut self) -> Vec<Range<usize>> {
        // Process any remaining incomplete line
        if !self.incomplete_line.is_empty() {
            self.query_lines.push(self.incomplete_line.clone());
            self.incomplete_line.clear();
            self.matches = self.resolve_location_fuzzy();
        }
        self.matches.clone()
    }

    fn resolve_location_fuzzy(&mut self) -> Vec<Range<usize>> {
        let new_query_line_count = self.query_lines.len();
        let old_query_line_count = self.matrix.rows.saturating_sub(1);
        if new_query_line_count == old_query_line_count {
            return Vec::new();
        }

        self.matrix.resize_rows(new_query_line_count + 1);

        // Process only the new query lines
        for row in old_query_line_count..new_query_line_count {
            let query_line = self.query_lines[row].trim();
            let leading_deletion_cost = (row + 1) as u32 * DELETION_COST;

            self.matrix.set(
                row + 1,
                0,
                SearchState::new(leading_deletion_cost, SearchDirection::Up),
            );

            let mut buffer_lines = self.snapshot.as_rope().chunks().lines();
            let mut col = 0;
            while let Some(buffer_line) = buffer_lines.next() {
                let buffer_line = buffer_line.trim();
                let up = SearchState::new(
                    self.matrix
                        .get(row, col + 1)
                        .cost
                        .saturating_add(DELETION_COST),
                    SearchDirection::Up,
                );
                let left = SearchState::new(
                    self.matrix
                        .get(row + 1, col)
                        .cost
                        .saturating_add(INSERTION_COST),
                    SearchDirection::Left,
                );
                let diagonal = SearchState::new(
                    if query_line == buffer_line {
                        self.matrix.get(row, col).cost
                    } else if fuzzy_eq(query_line, buffer_line) {
                        self.matrix.get(row, col).cost + REPLACEMENT_COST
                    } else {
                        self.matrix
                            .get(row, col)
                            .cost
                            .saturating_add(DELETION_COST + INSERTION_COST)
                    },
                    SearchDirection::Diagonal,
                );
                self.matrix
                    .set(row + 1, col + 1, up.min(left).min(diagonal));
                col += 1;
            }
        }

        // Find all matches with the best cost
        let buffer_line_count = self.snapshot.max_point().row as usize + 1;
        let mut best_cost = u32::MAX;
        let mut matches_with_best_cost = Vec::new();

        for col in 1..=buffer_line_count {
            let cost = self.matrix.get(new_query_line_count, col).cost;
            if cost < best_cost {
                best_cost = cost;
                matches_with_best_cost.clear();
                matches_with_best_cost.push(col as u32);
            } else if cost == best_cost {
                matches_with_best_cost.push(col as u32);
            }
        }

        // Find ranges for the matches
        let mut valid_matches = Vec::new();
        for &buffer_row_end in &matches_with_best_cost {
            let mut matched_lines = 0;
            let mut query_row = new_query_line_count;
            let mut buffer_row_start = buffer_row_end;
            while query_row > 0 && buffer_row_start > 0 {
                let current = self.matrix.get(query_row, buffer_row_start as usize);
                match current.direction {
                    SearchDirection::Diagonal => {
                        query_row -= 1;
                        buffer_row_start -= 1;
                        matched_lines += 1;
                    }
                    SearchDirection::Up => {
                        query_row -= 1;
                    }
                    SearchDirection::Left => {
                        buffer_row_start -= 1;
                    }
                }
            }

            let matched_buffer_row_count = buffer_row_end - buffer_row_start;
            let matched_ratio = matched_lines as f32
                / (matched_buffer_row_count as f32).max(new_query_line_count as f32);
            if matched_ratio >= 0.8 {
                let buffer_start_ix = self
                    .snapshot
                    .point_to_offset(Point::new(buffer_row_start, 0));
                let buffer_end_ix = self.snapshot.point_to_offset(Point::new(
                    buffer_row_end - 1,
                    self.snapshot.line_len(buffer_row_end - 1),
                ));
                valid_matches.push((buffer_row_start, buffer_start_ix..buffer_end_ix));
            }
        }

        valid_matches.into_iter().map(|(_, range)| range).collect()
    }

    /// Return the best match with starting position close enough to line_hint.
    pub fn select_best_match(&self) -> Option<Range<usize>> {
        // Allow line hint to be off by that many lines.
        // Higher values increase probability of applying edits to a wrong place,
        // Lower values increase edits failures and overall conversation length.
        const LINE_HINT_TOLERANCE: u32 = 200;

        if self.matches.is_empty() {
            return None;
        }

        if self.matches.len() == 1 {
            return self.matches.first().cloned();
        }

        let Some(line_hint) = self.line_hint else {
            // Multiple ambiguous matches
            return None;
        };

        let mut best_match = None;
        let mut best_distance = u32::MAX;

        for range in &self.matches {
            let start_point = self.snapshot.offset_to_point(range.start);
            let start_line = start_point.row;
            let distance = start_line.abs_diff(line_hint);

            if distance <= LINE_HINT_TOLERANCE && distance < best_distance {
                best_distance = distance;
                best_match = Some(range.clone());
            }
        }

        best_match
    }
}

fn fuzzy_eq(left: &str, right: &str) -> bool {
    const THRESHOLD: f64 = 0.8;

    let min_levenshtein = left.len().abs_diff(right.len());
    let min_normalized_levenshtein =
        1. - (min_levenshtein as f64 / cmp::max(left.len(), right.len()) as f64);
    if min_normalized_levenshtein < THRESHOLD {
        return false;
    }

    strsim::normalized_levenshtein(left, right) >= THRESHOLD
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SearchDirection {
    Up,
    Left,
    Diagonal,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SearchState {
    cost: u32,
    direction: SearchDirection,
}

impl SearchState {
    fn new(cost: u32, direction: SearchDirection) -> Self {
        Self { cost, direction }
    }
}

struct SearchMatrix {
    cols: usize,
    rows: usize,
    data: Vec<SearchState>,
}

impl SearchMatrix {
    fn new(cols: usize) -> Self {
        SearchMatrix {
            cols,
            rows: 0,
            data: Vec::new(),
        }
    }

    fn resize_rows(&mut self, needed_rows: usize) {
        debug_assert!(needed_rows > self.rows);
        self.rows = needed_rows;
        self.data.resize(
            self.rows * self.cols,
            SearchState::new(0, SearchDirection::Diagonal),
        );
    }

    fn get(&self, row: usize, col: usize) -> SearchState {
        debug_assert!(row < self.rows && col < self.cols);
        self.data[row * self.cols + col]
    }

    fn set(&mut self, row: usize, col: usize, state: SearchState) {
        debug_assert!(row < self.rows && col < self.cols);
        self.data[row * self.cols + col] = state;
    }
}
