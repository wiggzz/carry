//! Append-only terminal presentation; never switches to the alternate screen.
use std::io::IsTerminal;

#[derive(Default)]
pub struct Input {
    lines: Option<Vec<String>>,
}

pub enum Entry {
    Message(String),
    Notice(&'static str),
    Exit,
    Pending,
}

impl Input {
    pub fn line(&mut self, line: &str) -> Entry {
        if self.lines.is_some() {
            return match line {
                "/cancel" => {
                    self.lines = None;
                    Entry::Notice("Draft discarded.")
                }
                "/end" => {
                    let text = self.lines.take().unwrap().join("\n");
                    if text.trim().is_empty() {
                        Entry::Notice("Empty draft discarded.")
                    } else {
                        Entry::Message(text)
                    }
                }
                _ => {
                    self.lines.as_mut().unwrap().push(line.to_owned());
                    Entry::Pending
                }
            };
        }
        match line.trim() {
            "" => Entry::Pending,
            "/exit" | "/quit" => Entry::Exit,
            "/paste" => {
                self.lines = Some(Vec::new());
                Entry::Notice("Multiline input: /end on its own line sends; /cancel discards.")
            }
            "/help" => Entry::Notice(
                "Enter sends. /paste starts multiline input; /end sends; /cancel discards. /quit or /exit exits. EOF discards unfinished drafts.",
            ),
            _ => Entry::Message(line.to_owned()),
        }
    }
}

// Deliberately modest presentation: keep unsupported Markdown readable as source.
// Never interpret HTML or emit cursor movement / screen clearing sequences.
pub fn markdown(text: &str, color: bool) -> String {
    if !color {
        return text.to_owned();
    }
    let mut fence: Option<&str> = None;
    let mut output = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(marker) = fence {
            if trimmed.starts_with(marker) {
                fence = None;
                continue;
            }
            output.push(format!("\x1b[33m{line}\x1b[0m"));
        } else if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(if trimmed.starts_with("```") {
                "```"
            } else {
                "~~~"
            });
            let language = trimmed[3..].trim();
            if !language.is_empty() {
                output.push(format!("\x1b[2m{language}\x1b[0m"));
            }
        } else {
            let heading = trimmed.bytes().take_while(|c| *c == b'#').count();
            if (1..=6).contains(&heading) && trimmed[heading..].starts_with(' ') {
                output.push(format!("\x1b[1;36m{}\x1b[0m", &trimmed[heading + 1..]));
            } else if trimmed.starts_with("> ") {
                output.push(format!("\x1b[2m{line}\x1b[0m"));
            } else {
                output.push(inline(line));
            }
        }
    }
    let mut rendered = output.join("\n");
    if text.ends_with('\n') {
        rendered.push('\n');
    }
    rendered
}

fn inline(text: &str) -> String {
    let mut result = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let matched = [("`", "33"), ("**", "1"), ("__", "1")]
            .into_iter()
            .find_map(|(marker, style)| {
                let tail = rest.strip_prefix(marker)?;
                let end = tail.find(marker)?;
                (end > 0).then_some((marker, style, tail, end))
            });
        if let Some((marker, style, tail, end)) = matched {
            result.push_str(&format!("\x1b[{style}m{}\x1b[0m", &tail[..end]));
            rest = &tail[end + marker.len()..];
        } else {
            let ch = rest.chars().next().unwrap();
            result.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    result
}

pub fn print_answer(text: &str) {
    let color = std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").as_deref() != Ok("dumb");
    println!("{}", markdown(text, color));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_preserves_indentation_blank_lines_and_literal_commands() {
        let mut input = Input::default();
        assert!(matches!(input.line("/paste"), Entry::Notice(_)));
        for line in ["  first", "", "/quit", "last  "] {
            assert!(matches!(input.line(line), Entry::Pending));
        }
        assert!(matches!(input.line("/end"), Entry::Message(s) if s == "  first\n\n/quit\nlast  "));
        assert!(matches!(input.line("hello"), Entry::Message(s) if s == "hello"));
    }

    #[test]
    fn multiline_can_be_cancelled_without_sending() {
        let mut input = Input::default();
        input.line("/paste");
        input.line("draft");
        assert!(matches!(input.line("/cancel"), Entry::Notice(_)));
        assert!(matches!(input.line("/quit"), Entry::Exit));
    }

    #[test]
    fn markdown_styles_headings_and_code_without_rewriting_code() {
        let rendered = markdown("# Summary\n\n```rust\n  let x = 1;\n```", true);
        assert!(rendered.contains("\x1b[1;36mSummary\x1b[0m"));
        assert!(rendered.contains("  let x = 1;"));
        assert!(!rendered.contains("```"));
    }

    #[test]
    fn redirected_and_no_color_output_preserve_markdown() {
        let text = "# Heading\n**bold** and `code`\n";
        assert_eq!(markdown(text, false), text);
    }
}
