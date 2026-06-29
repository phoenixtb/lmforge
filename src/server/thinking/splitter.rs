/// Stateful streaming splitter for `<think>…</think>` blocks.
///
/// Feed it content chunks as they arrive from the SSE stream; it returns the
/// portion of each chunk that is reasoning vs. answer, correctly handling tag
/// markers that straddle chunk boundaries (e.g. `"<thi"` then `"nk>"`).
///
/// Used by:
/// - `proxy::stream_call1_accumulate` — collects reasoning from Call-1 of the
///   two-call budget orchestrator (llama.cpp / SGLang inline-tag path).
/// - `proxy::proxy_stream_rewriting_think_tags` — rewrites `<think>` inline
///   content to `reasoning_content` + `content` deltas in real-time.
#[derive(Default)]
pub struct ThinkSplitter {
    in_think: bool,
    /// Holds a trailing fragment that could be the start of a `<think>` /
    /// `</think>` marker split across chunks.
    pending: String,
}

impl ThinkSplitter {
    const OPEN: &'static str = "<think>";
    const CLOSE: &'static str = "</think>";

    /// Longest suffix of `buf` that is a proper prefix of `tag` (so we can
    /// hold it back until the next chunk completes the potential marker).
    fn partial_tail(buf: &str, tag: &str) -> usize {
        let max = buf.len().min(tag.len() - 1);
        for n in (1..=max).rev() {
            if buf.is_char_boundary(buf.len() - n) && tag.starts_with(&buf[buf.len() - n..]) {
                return n;
            }
        }
        0
    }

    /// Push a content chunk. Returns `(reasoning, answer)` extracted from it.
    pub fn push(&mut self, chunk: &str) -> (String, String) {
        let mut buf = std::mem::take(&mut self.pending);
        buf.push_str(chunk);
        let mut reasoning = String::new();
        let mut answer = String::new();

        loop {
            if self.in_think {
                if let Some(pos) = buf.find(Self::CLOSE) {
                    reasoning.push_str(&buf[..pos]);
                    buf.drain(..pos + Self::CLOSE.len());
                    self.in_think = false;
                } else {
                    let keep = Self::partial_tail(&buf, Self::CLOSE);
                    let cut = buf.len() - keep;
                    reasoning.push_str(&buf[..cut]);
                    self.pending = buf[cut..].to_string();
                    break;
                }
            } else if let Some(pos) = buf.find(Self::OPEN) {
                answer.push_str(&buf[..pos]);
                buf.drain(..pos + Self::OPEN.len());
                self.in_think = true;
            } else {
                let keep = Self::partial_tail(&buf, Self::OPEN);
                let cut = buf.len() - keep;
                answer.push_str(&buf[..cut]);
                self.pending = buf[cut..].to_string();
                break;
            }
        }
        (reasoning, answer)
    }

    /// Flush any held-back fragment at stream end. Emits it as reasoning if
    /// the splitter ended inside an unterminated `<think>` (budget exhausted),
    /// otherwise as answer.
    pub fn flush(&mut self) -> (String, String) {
        let rest = std::mem::take(&mut self.pending);
        if self.in_think {
            (rest, String::new())
        } else {
            (String::new(), rest)
        }
    }

    /// True if the splitter ended while still inside an unterminated `<think>`.
    pub fn unterminated(&self) -> bool {
        self.in_think
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn splitter_collect(chunks: &[&str]) -> (String, String, bool) {
        let mut s = ThinkSplitter::default();
        let (mut r, mut a) = (String::new(), String::new());
        for c in chunks {
            let (rr, aa) = s.push(c);
            r.push_str(&rr);
            a.push_str(&aa);
        }
        let (rr, aa) = s.flush();
        r.push_str(&rr);
        a.push_str(&aa);
        (r, a, s.unterminated())
    }

    #[test]
    fn test_splitter_single_chunk() {
        let (r, a, unterm) = splitter_collect(&["<think>reasoning here</think>the answer"]);
        assert_eq!(r, "reasoning here");
        assert_eq!(a, "the answer");
        assert!(!unterm);
    }

    #[test]
    fn test_splitter_no_think() {
        let (r, a, _) = splitter_collect(&["just a plain answer"]);
        assert_eq!(r, "");
        assert_eq!(a, "just a plain answer");
    }

    #[test]
    fn test_splitter_open_tag_split_across_chunks() {
        let (r, a, _) = splitter_collect(&["<thi", "nk>reason", "</thi", "nk>ans"]);
        assert_eq!(r, "reason");
        assert_eq!(a, "ans");
    }

    #[test]
    fn test_splitter_unterminated_think_is_reasoning() {
        let (r, a, unterm) = splitter_collect(&["<think>still thinking and loop", "ing forever"]);
        assert_eq!(r, "still thinking and looping forever");
        assert_eq!(a, "");
        assert!(unterm);
    }

    #[test]
    fn test_splitter_token_by_token() {
        let src = "<think>ab</think>cd";
        let mut s = ThinkSplitter::default();
        let (mut r, mut a) = (String::new(), String::new());
        for ch in src.chars() {
            let (rr, aa) = s.push(&ch.to_string());
            r.push_str(&rr);
            a.push_str(&aa);
        }
        let (rr, aa) = s.flush();
        r.push_str(&rr);
        a.push_str(&aa);
        assert_eq!(r, "ab");
        assert_eq!(a, "cd");
    }
}
