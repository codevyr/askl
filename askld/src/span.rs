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

    pub fn as_pest_span(&self) -> pest::Span<'_> {
        pest::Span::new(&self.input, self.start, self.end).expect("valid span")
    }

    pub fn input(&self) -> Arc<String> {
        self.input.clone()
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
