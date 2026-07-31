use core::fmt;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Span {
    input: Arc<String>,
    start: usize,
    end: usize,
}

impl Span {
    pub fn from_pest(span: pest::Span<'_>, input: Arc<String>) -> Self {
        Self {
            input,
            start: span.start(),
            end: span.end(),
        }
    }

    pub fn entire(input: Arc<String>) -> Self {
        let len = input.len();
        Self {
            input,
            start: 0,
            end: len,
        }
    }

    pub fn synthetic(text: impl Into<String>) -> Self {
        let input = Arc::new(text.into());
        let len = input.len();
        Self {
            input,
            start: 0,
            end: len,
        }
    }

    pub fn start(&self) -> usize {
        self.start
    }

    pub fn end(&self) -> usize {
        self.end
    }

    pub fn as_pest_span(&self) -> pest::Span<'_> {
        pest::Span::new(&self.input, self.start, self.end).expect("valid span")
    }

    pub fn input(&self) -> Arc<String> {
        self.input.clone()
    }

    /// The source text this span covers.
    pub fn as_str(&self) -> &str {
        &self.input[self.start..self.end]
    }

    /// The full source line containing the span's start.
    pub fn line_text(&self) -> &str {
        let line_start = self.input[..self.start]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let line_end = self.input[self.start..]
            .find('\n')
            .map(|i| self.start + i)
            .unwrap_or(self.input.len());
        &self.input[line_start..line_end]
    }

    pub fn sub_span(&self, start: usize, end: usize) -> Self {
        Self {
            input: self.input.clone(),
            start,
            end,
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pest_span = self.as_pest_span();
        let (start_line, start_col) = pest_span.start_pos().line_col();
        let (end_line, end_col) = pest_span.end_pos().line_col();
        if start_line == end_line {
            write!(f, "Line {}:{}-{}", start_line, start_col, end_col)
        } else {
            write!(
                f,
                "Line {}:{} to Line {}:{}",
                start_line, start_col, end_line, end_col
            )
        }
    }
}
