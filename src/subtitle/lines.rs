#[derive(Clone, Copy, Debug)]
pub struct Line {
    pub start: usize,
    /// Excludes `\r` / `\n`.
    pub end: usize,
    /// Includes the complete line ending, if any.
    pub end_with_ending: usize,
}

impl Line {
    pub fn text<'a>(&self, input: &'a str) -> &'a str {
        &input[self.start..self.end]
    }

    pub fn is_blank(&self, input: &str) -> bool {
        self.text(input).trim().is_empty()
    }
}

pub fn split(input: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in input.bytes().enumerate() {
        if byte == b'\n' {
            let end = if index > start && input.as_bytes()[index - 1] == b'\r' {
                index - 1
            } else {
                index
            };
            lines.push(Line {
                start,
                end,
                end_with_ending: index + 1,
            });
            start = index + 1;
        }
    }
    if start < input.len() {
        lines.push(Line {
            start,
            end: input.len(),
            end_with_ending: input.len(),
        });
    }
    lines
}
