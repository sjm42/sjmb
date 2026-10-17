// html_title.rs

//! Bounded title extraction without building or recursively walking a DOM.

use html5ever::tokenizer::{
    BufferQueue, CharacterTokens, EndTag, StartTag, TagToken, Token, TokenSink, TokenSinkResult, Tokenizer,
    states::RawKind,
};

use crate::*;

// Leave room for an ellipsis and the surrounding quotes in the IRC payload.
const TITLE_BYTES: usize = 394;

#[derive(Default)]
struct TitleState {
    active: bool,
    done: bool,
    pending_space: bool,
    truncated: bool,
    title: String,
}

impl TitleState {
    fn process_token(&mut self, token: Token) -> TokenSinkResult<()> {
        if self.done {
            return TokenSinkResult::Continue;
        }
        match token {
            TagToken(tag) if tag.kind == StartTag => match tag.name.as_ref() {
                "title" => {
                    self.active = true;
                    return TokenSinkResult::RawData(RawKind::Rcdata);
                }
                "script" => return TokenSinkResult::RawData(RawKind::ScriptData),
                "style" | "xmp" | "iframe" | "noembed" | "noframes" => {
                    return TokenSinkResult::RawData(RawKind::Rawtext);
                }
                "textarea" => return TokenSinkResult::RawData(RawKind::Rcdata),
                "plaintext" => return TokenSinkResult::Plaintext,
                _ => {}
            },
            TagToken(tag) if tag.kind == EndTag && &*tag.name == "title" && self.active => {
                self.active = false;
                self.done = true;
            }
            CharacterTokens(text) if self.active => {
                for ch in text.chars() {
                    if ch.is_whitespace() {
                        self.pending_space = !self.title.is_empty();
                    } else if !ch.is_control() {
                        let needed = ch.len_utf8() + usize::from(self.pending_space);
                        if self.title.len() + needed > TITLE_BYTES {
                            self.truncated = true;
                            self.done = true;
                            break;
                        }
                        if self.pending_space {
                            self.title.push(' ');
                            self.pending_space = false;
                        }
                        self.title.push(ch);
                    }
                }
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

#[derive(Default)]
struct TitleSink {
    state: Cell<TitleState>,
    done: Cell<bool>,
}

impl TokenSink for TitleSink {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        // html5ever's sink API takes &self. Move the state through a Cell rather
        // than relying on runtime borrow checks or sharing it across threads.
        let mut state = self.state.take();
        let result = state.process_token(token);
        self.done.set(state.done);
        self.state.set(state);
        result
    }
}

pub fn extract_title(body: &str) -> Option<String> {
    let tokenizer = Tokenizer::new(TitleSink::default(), Default::default());
    // Feed small chunks so a title near the start avoids scanning the remaining body.
    let mut offset = 0;
    let queue = BufferQueue::default();
    while offset < body.len() && !tokenizer.sink.done.get() {
        let end = body.floor_char_boundary((offset + 4096).min(body.len()));
        queue.push_back(body[offset..end].into());
        let _ = tokenizer.feed(&queue);
        offset = end;
    }
    tokenizer.end();
    let sink = tokenizer.sink.state.into_inner();
    if sink.title.is_empty() {
        None
    } else if sink.truncated {
        Some(format!("{}...", sink.title))
    } else {
        Some(sink.title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_real_title_with_entities_and_collapsed_space() {
        assert_eq!(
            extract_title(
                "<!-- <title>fake</title> --><script>'<title>fake</title>'</script><TITLE> Fish &amp;\n chips </TITLE>"
            ),
            Some("Fish & chips".into())
        );
    }

    #[test]
    fn ignores_unicode_metadata_and_deep_nesting() {
        let html = format!(
            "<meta property='og:image' content='x'><meta property='og:imageé' content='x'>{}<title>safe</title>{}",
            "<div>".repeat(10_000),
            "</div>".repeat(10_000)
        );
        assert_eq!(extract_title(&html), Some("safe".into()));
    }

    #[test]
    fn bounds_unicode_output_and_accepts_a_truncated_page() {
        let title = extract_title(&format!("<title>{}", "é".repeat(1000))).unwrap();
        assert!(title.len() <= 400);
        assert!(title.ends_with("..."));
        assert_eq!(extract_title("<title>partial"), Some("partial".into()));
        assert_eq!(extract_title("<body>no title"), None);
    }

    #[test]
    fn preserves_entities_and_tags_across_feed_boundaries() {
        for padding in 4080..4100 {
            let html = format!("{}<title>A &amp; B</title>", " ".repeat(padding));
            assert_eq!(extract_title(&html), Some("A & B".into()), "padding={padding}");
        }
    }
}
// EOF
