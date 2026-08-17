//! Tokenisation for subtitle payloads.
//!
//! A translator receives only [`TextTemplate::plain_text`].  Formatting tokens
//! are reinserted afterwards at a proportional position, so a model cannot
//! accidentally edit or remove ASS override tags / `WebVTT` markup.

#[derive(Clone, Debug)]
enum Fragment {
    Text(String),
    Protected(String),
}

#[derive(Clone, Debug)]
pub struct TextTemplate {
    fragments: Vec<Fragment>,
    plain_text: String,
}

impl TextTemplate {
    pub fn plain(input: &str) -> Self {
        Self {
            fragments: vec![Fragment::Text(input.into())],
            plain_text: input.into(),
        }
    }

    /// Preserve simple inline tags used by SRT and `WebVTT` (`<i>`, `<c.foo>`,
    /// timestamp tags, …).  Unclosed `<` is normal text.
    pub fn with_markup(input: &str) -> Self {
        let mut fragments = Vec::new();
        let mut plain_text = String::new();
        let mut text_start = 0;
        let bytes = input.as_bytes();
        let mut cursor = 0;
        while cursor < bytes.len() {
            if bytes[cursor] == b'<'
                && let Some(relative_end) = input[cursor + 1..].find('>')
            {
                let end = cursor + 1 + relative_end + 1;
                push_text(input, text_start, cursor, &mut fragments, &mut plain_text);
                fragments.push(Fragment::Protected(input[cursor..end].into()));
                cursor = end;
                text_start = end;
                continue;
            }
            cursor += 1;
        }
        push_text(
            input,
            text_start,
            input.len(),
            &mut fragments,
            &mut plain_text,
        );
        Self {
            fragments,
            plain_text,
        }
    }

    /// Preserve ASS override blocks and layout escape sequences.  `\N`, `\n`,
    /// and `\h` influence layout and therefore must not be handed to an LLM as
    /// editable content.
    pub fn with_ass_overrides(input: &str) -> Self {
        let mut fragments = Vec::new();
        let mut plain_text = String::new();
        let bytes = input.as_bytes();
        let mut text_start = 0;
        let mut cursor = 0;
        while cursor < bytes.len() {
            if bytes[cursor] == b'{'
                && let Some(relative_end) = input[cursor + 1..].find('}')
            {
                let end = cursor + 1 + relative_end + 1;
                push_text(input, text_start, cursor, &mut fragments, &mut plain_text);
                fragments.push(Fragment::Protected(input[cursor..end].into()));
                cursor = end;
                text_start = end;
                continue;
            }
            if bytes[cursor] == b'\\' && cursor + 1 < bytes.len() {
                let code = bytes[cursor + 1];
                if matches!(code, b'N' | b'n' | b'h') {
                    push_text(input, text_start, cursor, &mut fragments, &mut plain_text);
                    fragments.push(Fragment::Protected(input[cursor..cursor + 2].into()));
                    cursor += 2;
                    text_start = cursor;
                    continue;
                }
            }
            cursor += 1;
        }
        push_text(
            input,
            text_start,
            input.len(),
            &mut fragments,
            &mut plain_text,
        );
        Self {
            fragments,
            plain_text,
        }
    }

    pub fn plain_text(&self) -> &str {
        &self.plain_text
    }

    pub fn render(&self, translated: &str) -> String {
        self.render_inner(&escape_markup(translated))
    }

    pub fn render_ass(&self, translated: &str) -> String {
        self.render_inner(&escape_ass_override_braces(translated))
    }

    fn render_inner(&self, translated: &str) -> String {
        if self.plain_text.is_empty() {
            return self.original();
        }
        let source_len = self.plain_text.chars().count();
        let target_chars: Vec<char> = translated.chars().collect();
        let target_len = target_chars.len();
        let mut inserts: Vec<String> = vec![String::new(); target_len + 1];
        let mut source_offset = 0usize;
        for fragment in &self.fragments {
            match fragment {
                Fragment::Text(value) => source_offset += value.chars().count(),
                Fragment::Protected(value) => {
                    let target_offset = source_offset.saturating_mul(target_len) / source_len;
                    inserts[target_offset].push_str(value);
                }
            }
        }

        let mut result = String::new();
        for (index, character) in target_chars.into_iter().enumerate() {
            result.push_str(&inserts[index]);
            result.push(character);
        }
        result.push_str(&inserts[target_len]);
        result
    }

    fn original(&self) -> String {
        let mut value = String::new();
        for fragment in &self.fragments {
            match fragment {
                Fragment::Text(part) | Fragment::Protected(part) => value.push_str(part),
            }
        }
        value
    }
}

fn push_text(
    input: &str,
    start: usize,
    end: usize,
    fragments: &mut Vec<Fragment>,
    plain_text: &mut String,
) {
    if start < end {
        let text = &input[start..end];
        plain_text.push_str(text);
        fragments.push(Fragment::Text(text.into()));
    }
}

fn escape_markup(value: &str) -> String {
    value.replace('<', "&lt;").replace('>', "&gt;")
}

fn escape_ass_override_braces(value: &str) -> String {
    // New braces could create an ASS override tag.  They are intentionally
    // neutralised rather than trusted as provider output.
    value.replace('{', "｛").replace('}', "｝")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ass_tags_are_excluded_and_restored() {
        let template =
            TextTemplate::with_ass_overrides(r"{\an8}私の名前は{\c&H00FFFF&}Alice{\c}です");
        assert_eq!(template.plain_text(), "私の名前はAliceです");
        let rendered = template.render_ass("我的名字是 Alice");
        assert!(rendered.contains(r"{\an8}"));
        assert!(rendered.contains(r"{\c&H00FFFF&}"));
        assert!(rendered.contains(r"{\c}"));
    }

    #[test]
    fn provider_cannot_add_ass_override_tags() {
        let template = TextTemplate::with_ass_overrides("hello");
        assert_eq!(template.render_ass(r"{\bord99}hello"), "｛\\bord99｝hello");
    }

    #[test]
    fn common_ass_layout_and_style_tags_survive_translation() {
        let template = TextTemplate::with_ass_overrides(
            r"{\an8}{\pos(100,200)}A{\move(1,2,3,4)}B{\c&H00FFFF&}C{\bord2}D{\c}",
        );
        let rendered = template.render_ass("translated text");
        for tag in [
            r"{\an8}",
            r"{\pos(100,200)}",
            r"{\move(1,2,3,4)}",
            r"{\c&H00FFFF&}",
            r"{\bord2}",
            r"{\c}",
        ] {
            assert!(rendered.contains(tag), "missing {tag} in {rendered}");
        }
    }
}
