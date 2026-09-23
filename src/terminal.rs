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
                "Enter sends; Alt+Enter or Ctrl+J inserts a newline; paste inserts without sending. Ctrl+C clears the draft. /paste also starts multiline input; /end sends; /cancel discards. /quit or /exit exits. EOF discards unfinished drafts.",
            ),
            _ => Entry::Message(line.to_owned()),
        }
    }
}

#[derive(Default)]
struct MarkdownRenderer {
    fence: Option<String>,
}

impl MarkdownRenderer {
    fn render_line(&mut self, line: &str, color: bool) -> Option<String> {
        if !color {
            return Some(line.to_owned());
        }
        let trimmed = line.trim_start();
        if let Some(marker) = self.fence.as_deref() {
            if trimmed.starts_with(marker) {
                self.fence = None;
                None
            } else {
                Some(format!("\x1b[33m{line}\x1b[0m"))
            }
        } else if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let marker = if trimmed.starts_with("```") {
                "```"
            } else {
                "~~~"
            };
            self.fence = Some(marker.to_owned());
            let language = trimmed[3..].trim();
            (!language.is_empty()).then(|| format!("\x1b[2m{language}\x1b[0m"))
        } else {
            let heading = trimmed.bytes().take_while(|c| *c == b'#').count();
            if (1..=6).contains(&heading) && trimmed[heading..].starts_with(' ') {
                Some(format!("\x1b[1;36m{}\x1b[0m", &trimmed[heading + 1..]))
            } else if trimmed.starts_with("> ") {
                Some(format!("\x1b[2m{line}\x1b[0m"))
            } else {
                Some(inline(line))
            }
        }
    }
}

// Deliberately modest presentation: keep unsupported Markdown readable as source.
// Never interpret HTML or emit cursor movement / screen clearing sequences.
pub fn markdown(text: &str, color: bool) -> String {
    if !color {
        return text.to_owned();
    }
    let mut renderer = MarkdownRenderer::default();
    let mut output = Vec::new();
    for line in text.lines() {
        if let Some(line) = renderer.render_line(line, true) {
            output.push(line);
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

fn color_enabled(is_terminal: bool) -> bool {
    is_terminal
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").as_deref() != Ok("dumb")
}

pub fn print_answer(text: &str) {
    let rendered = markdown(text, color_enabled(std::io::stdout().is_terminal()));
    if std::io::stdout().is_terminal() && editor_active() {
        output(&rendered);
    } else {
        println!("{rendered}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static STREAM_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    #[test]
    fn streamed_markdown_preserves_blank_lines_and_fenced_code_styling() {
        let _serial = STREAM_TEST_LOCK.lock().unwrap();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = captured.clone();
        *PRINTER.lock().unwrap() = Some(Box::new(move |message| {
            for line in message.lines() {
                sink.lock().unwrap().push(line.to_owned());
            }
        }));

        let mut output = StreamOutput::with_color(true);
        output.push("# Heading\n\n```rust\n  let x = 1;\n```\n");
        output.finish();
        *PRINTER.lock().unwrap() = None;

        assert_eq!(
            *captured.lock().unwrap(),
            vec![
                "\x1b[1;36mHeading\x1b[0m",
                "",
                "\x1b[2mrust\x1b[0m",
                "\x1b[33m  let x = 1;\x1b[0m",
            ]
        );
    }
}

/// Redirected stdout must always receive the complete answer, even when stderr
/// was used for a live preview.
pub fn should_print_answer(answer: &str, streamed: &str, stdout_is_terminal: bool) -> bool {
    !stdout_is_terminal || streamed.is_empty() || answer != streamed
}

#[cfg(test)]
mod streaming_tests {
    use super::*;

    #[test]
    fn completed_stream_is_not_printed_twice_but_redirects_and_partial_streams_are_complete() {
        assert!(!should_print_answer("hello", "hello", true));
        assert!(should_print_answer("hello", "hello", false));
        assert!(should_print_answer("hello", "hel", true));
        assert!(should_print_answer("hello", "", true));
    }
}

type Printer = Box<dyn FnMut(String) + Send>;
static PRINTER: std::sync::Mutex<Option<Printer>> = std::sync::Mutex::new(None);

pub fn output(message: &str) {
    let mut printer = PRINTER.lock().unwrap();
    if let Some(print) = printer.as_mut() {
        print(message.to_owned());
    } else {
        eprintln!("{message}");
    }
}

pub fn editor_active() -> bool {
    PRINTER.lock().unwrap().is_some()
}

pub fn read_input(sender: tokio::sync::mpsc::UnboundedSender<crate::run::UserInput>) {
    let result = if std::io::stdout().is_terminal() && std::io::stderr().is_terminal() {
        read_reedline_input(&sender)
    } else {
        read_basic_input(&sender)
    };
    *PRINTER.lock().unwrap() = None;
    if let Err(error) = result {
        eprintln!("terminal editor failed: {error}");
    }
    let _ = sender.send(crate::run::UserInput::Exit);
}

fn read_basic_input(
    sender: &tokio::sync::mpsc::UnboundedSender<crate::run::UserInput>,
) -> std::io::Result<()> {
    use std::io::{BufRead, Write};

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut input = Input::default();
    loop {
        eprint!("carry> ");
        std::io::stderr().flush()?;
        let Some(line) = lines.next() else { break };
        match input.line(&line?) {
            Entry::Message(message) => {
                if sender
                    .send(crate::run::UserInput::Message {
                        message,
                        submission_id: None,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Entry::Notice(message) => output(message),
            Entry::Exit => break,
            Entry::Pending => {}
        }
    }
    Ok(())
}

fn read_reedline_input(
    sender: &tokio::sync::mpsc::UnboundedSender<crate::run::UserInput>,
) -> std::io::Result<()> {
    use reedline::{
        DefaultPrompt, DefaultPromptSegment, EditCommand, Emacs, ExternalPrinter, KeyCode,
        KeyModifiers, Reedline, ReedlineEvent, Signal, default_emacs_keybindings,
    };
    let mut keys = default_emacs_keybindings();
    for (modifiers, key) in [
        (KeyModifiers::ALT, KeyCode::Enter),
        (KeyModifiers::SHIFT, KeyCode::Enter),
        (KeyModifiers::CONTROL, KeyCode::Char('j')),
    ] {
        keys.add_binding(
            modifiers,
            key,
            ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
        );
    }
    let printer = ExternalPrinter::default();
    let mut editor = Reedline::create()
        .with_edit_mode(Box::new(Emacs::new(keys)))
        .use_bracketed_paste(true)
        .with_external_printer(printer.clone())
        .with_ansi_colors(std::env::var_os("NO_COLOR").is_none());
    *PRINTER.lock().unwrap() = Some(Box::new(move |message| {
        let _ = printer.print(message);
    }));
    let prompt = DefaultPrompt {
        left_prompt: DefaultPromptSegment::Basic("carry".into()),
        right_prompt: DefaultPromptSegment::Empty,
    };
    let mut input = Input::default();
    loop {
        let line = match editor.read_line(&prompt)? {
            Signal::Success(line) => line,
            Signal::CtrlC => {
                input = Input::default();
                continue;
            }
            Signal::CtrlD => break,
        };
        match input.line(&line) {
            Entry::Message(message) => {
                if sender
                    .send(crate::run::UserInput::Message {
                        message,
                        submission_id: None,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Entry::Notice(message) => output(message),
            Entry::Exit => break,
            Entry::Pending => {}
        }
    }
    Ok(())
}

/// External printers work in lines: buffer a partial line rather than repainting
/// the input for every token or putting every fragment on its own line.
pub struct StreamOutput {
    pending: String,
    renderer: MarkdownRenderer,
    color: bool,
}

impl Default for StreamOutput {
    fn default() -> Self {
        Self::with_color(color_enabled(std::io::stderr().is_terminal()))
    }
}

impl StreamOutput {
    fn with_color(color: bool) -> Self {
        Self {
            pending: String::new(),
            renderer: MarkdownRenderer::default(),
            color,
        }
    }

    fn output_complete_line(&mut self, line: &str) {
        if let Some(mut rendered) = self.renderer.render_line(line, self.color) {
            rendered.push('\n');
            output(&rendered);
        }
    }

    pub fn push(&mut self, delta: &str) {
        use std::io::Write;
        if editor_active() {
            self.pending.push_str(delta);
            while let Some(end) = self.pending.find('\n') {
                let line: String = self.pending.drain(..=end).collect();
                self.output_complete_line(line.trim_end_matches('\n'));
            }
        } else {
            eprint!("{delta}");
            let _ = std::io::stderr().flush();
        }
    }
    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            if let Some(rendered) = self.renderer.render_line(&pending, self.color) {
                output(&rendered);
            }
        } else if !editor_active() {
            eprintln!();
        }
    }
}
