//! §2 chunking: folded event text → embeddable units.
//!
//! Contract: spec/RETRIEVAL.md §2. All offsets are Unicode-scalar (char)
//! indices into the folded text, so provenance can quote the exact matched
//! span (§6) regardless of byte encoding.

/// Version of the tiny-chunk context-prefix scheme (§2). An `inputs_hash`
/// input (§1.2): bumping it invalidates and re-embeds every chunk rather
/// than silently mixing prefix recipes.
pub const PREFIX_SCHEME_VERSION: u32 = 1;

/// ~512-token chunk target (§2: "512 is a target, not a contract").
const TARGET_TOKENS: usize = 512;
/// 64-token overlap between consecutive chunks of a long monologue.
const OVERLAP_TOKENS: usize = 64;
/// §2 heuristic when the connector exposes no tokenizer:
/// `tokens ~= whitespace_words * 1.3`. Expressed as word budgets so the
/// chunker never needs a tokenizer at all in v1.
const TOKENS_PER_WORD: f64 = 1.3;

fn words_for_tokens(tokens: usize) -> usize {
    // floor: erring small keeps chunks under the model context.
    ((tokens as f64) / TOKENS_PER_WORD) as usize
}

/// One embeddable unit of one event's folded text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// 0..n within one event's text (`vectors.chunk_index`).
    pub index: u32,
    /// Unicode-scalar offsets into the folded text (`char_start`/`char_end`).
    pub char_start: u32,
    pub char_end: u32,
    /// The bare folded-text span — what provenance quotes (§6). Never
    /// carries the context prefix.
    pub text: String,
    /// What actually goes to `Embedder::embed_text`: the span, with the
    /// deterministic tiny-chunk metadata prefix prepended when the span is
    /// under ~2 sentences (§2). The prefix exists at embed time only.
    pub embed_text: String,
}

/// Deterministic metadata for the tiny-chunk context prefix (§2):
/// capture/annotation date, folder name, and active collection name if any,
/// rendered as `[2026-01-14 · 2026/iceland · Quiet Hours] `. MUST stay
/// deterministic — no generated text — so rebuild byte-equality (§13.8)
/// is preserved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChunkContext {
    /// `YYYY-MM-DD`.
    pub date: Option<String>,
    pub folder: Option<String>,
    /// Active collection name (collections store lands in P7.3; callers
    /// pass `None` until then).
    pub collection: Option<String>,
}

impl ChunkContext {
    /// The rendered prefix, or `None` when no metadata part is present.
    fn prefix(&self) -> Option<String> {
        let parts: Vec<&str> = [&self.date, &self.folder, &self.collection]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(format!("[{}] ", parts.join(" \u{b7} ")))
        }
    }
}

/// Chunk one event's folded text per §2.
///
/// - <= ~512 tokens: exactly one chunk spanning the whole text — the
///   overwhelmingly common case.
/// - Longer: ~512-token chunks with 64-token overlap; boundaries snap
///   backward to the nearest sentence end within the window's final 64
///   tokens when one exists, else hard-split.
pub fn chunk_folded_text(folded: &str, ctx: &ChunkContext) -> Vec<Chunk> {
    let chars: Vec<char> = folded.chars().collect();
    let words = word_spans(&chars);
    if words.is_empty() {
        return Vec::new();
    }

    let estimated_tokens = (words.len() as f64 * TOKENS_PER_WORD).ceil() as usize;
    if estimated_tokens <= TARGET_TOKENS {
        // One chunk, offsets spanning the whole text (§2).
        return vec![make_chunk(0, 0, chars.len(), &chars, ctx)];
    }

    let target_words = words_for_tokens(TARGET_TOKENS);
    let overlap_words = words_for_tokens(OVERLAP_TOKENS);
    let mut chunks = Vec::new();
    let mut start_word = 0usize;
    loop {
        let mut end_word = (start_word + target_words).min(words.len());
        if end_word < words.len() {
            // Snap backward to the nearest sentence end within the final
            // 64 tokens of the window, when one exists.
            let snap_floor = end_word.saturating_sub(overlap_words).max(start_word + 1);
            for j in (snap_floor..=end_word).rev() {
                if ends_sentence(&chars, &words[j - 1]) {
                    end_word = j;
                    break;
                }
            }
        }
        let span_start = words[start_word].0;
        let span_end = words[end_word - 1].1;
        chunks.push(make_chunk(
            chunks.len() as u32,
            span_start,
            span_end,
            &chars,
            ctx,
        ));
        if end_word == words.len() {
            break;
        }
        // 64-token overlap; always make forward progress.
        start_word = end_word.saturating_sub(overlap_words).max(start_word + 1);
    }
    chunks
}

/// Maximal runs of non-whitespace, as `(start_char, end_char)` spans.
fn word_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in chars.iter().enumerate() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                spans.push((s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        spans.push((s, chars.len()));
    }
    spans
}

/// Does this word end a sentence? Terminal punctuation, looking through
/// closing quotes/brackets ("ridge.)" still ends a sentence).
fn ends_sentence(chars: &[char], word: &(usize, usize)) -> bool {
    chars[word.0..word.1]
        .iter()
        .rev()
        .find(|c| {
            !matches!(
                c,
                ')' | ']' | '"' | '\'' | '\u{201d}' | '\u{2019}' | '\u{bb}'
            )
        })
        .is_some_and(|c| matches!(c, '.' | '!' | '?' | '\u{2026}'))
}

fn make_chunk(
    index: u32,
    char_start: usize,
    char_end: usize,
    chars: &[char],
    ctx: &ChunkContext,
) -> Chunk {
    let text: String = chars[char_start..char_end].iter().collect();
    // "Under ~2 sentences": fewer than two sentence-terminal words. A
    // terminator-free fragment ("love this one") counts as under two.
    let sentence_ends = word_spans(&chars[char_start..char_end])
        .iter()
        .filter(|w| ends_sentence(&chars[char_start..char_end], w))
        .count();
    let embed_text = match ctx.prefix() {
        Some(prefix) if sentence_ends < 2 => format!("{prefix}{text}"),
        _ => text.clone(),
    };
    Chunk {
        index,
        char_start: char_start as u32,
        char_end: char_end as u32,
        text,
        embed_text,
    }
}
