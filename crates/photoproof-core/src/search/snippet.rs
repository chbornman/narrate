//! Snippet → provenance mapping (RETRIEVAL §4, §5.4, §6): the FTS5
//! `snippet()` output with `⟦`/`⟧` sentinels becomes the Quote's window
//! text, its char span within the folded text, and highlight ranges.
//!
//! All offsets are Unicode-scalar (char) counts per §1.2. The sentinels are
//! mapped here, in core — never raw HTML through IPC (§4).

/// Sentinels and ellipsis exactly as the §4 statement passes them.
pub(crate) const OPEN: char = '⟦';
pub(crate) const CLOSE: char = '⟧';
pub(crate) const ELLIPSIS: char = '…';

pub(crate) struct MappedSnippet {
    /// The snippet window: an exact substring of the folded text.
    pub text: String,
    /// Span of `text` within the folded text, in chars.
    pub char_start: u32,
    pub char_end: u32,
    /// Matched-term ranges within `text`, in chars.
    pub highlights: Vec<(u32, u32)>,
}

/// Map a raw `snippet()` string onto the folded text it was cut from.
///
/// `exact_terms`/`prefix_term` are the lowercased query tokens (§4
/// construction): a highlight whose token equals an exact term keeps the
/// whole token; one matched only by the prefix term is trimmed to the typed
/// prefix (§7.2 normative display: `⟦ba⟧rn` for `"ba"*`); anything else —
/// e.g. a diacritic-folded match the lowercase comparison cannot see —
/// keeps the conservative whole-token highlight.
///
/// Fallback: if the window cannot be located in the folded text (which the
/// transactional index maintenance should make impossible), the whole folded
/// text is quoted with no highlights — still verbatim user words, never a
/// fabricated span.
pub(crate) fn map_snippet(
    folded: &str,
    snip: &str,
    exact_terms: &[String],
    prefix_term: Option<&str>,
) -> MappedSnippet {
    // 1. Strip edge ellipses (inserted only when the window is truncated).
    let core = snip.strip_prefix(ELLIPSIS).unwrap_or(snip);
    let core = core.strip_suffix(ELLIPSIS).unwrap_or(core);

    // 2. Remove sentinels, recording highlight char ranges in window space.
    let mut window = String::with_capacity(core.len());
    let mut highlights: Vec<(u32, u32)> = Vec::new();
    let mut open_at: Option<u32> = None;
    let mut chars: u32 = 0;
    for c in core.chars() {
        match c {
            OPEN => open_at = Some(chars),
            CLOSE => {
                if let Some(start) = open_at.take() {
                    highlights.push((start, chars));
                }
            }
            _ => {
                window.push(c);
                chars += 1;
            }
        }
    }

    // 3. Locate the window in the folded text (it is an exact substring).
    let Some(byte_start) = folded.find(&window) else {
        return MappedSnippet {
            text: folded.to_owned(),
            char_start: 0,
            char_end: folded.chars().count() as u32,
            highlights: Vec::new(),
        };
    };
    let char_start = folded[..byte_start].chars().count() as u32;
    let char_end = char_start + chars;

    // 4. Trim prefix-term highlights to the typed prefix.
    let trimmed = highlights
        .into_iter()
        .map(|(hs, he)| trim_highlight(&window, hs, he, exact_terms, prefix_term))
        .collect();

    MappedSnippet {
        text: window,
        char_start,
        char_end,
        highlights: trimmed,
    }
}

fn trim_highlight(
    window: &str,
    hs: u32,
    he: u32,
    exact_terms: &[String],
    prefix_term: Option<&str>,
) -> (u32, u32) {
    let token: String = window
        .chars()
        .skip(hs as usize)
        .take((he - hs) as usize)
        .collect();
    let lower = token.to_lowercase();
    if exact_terms.contains(&lower) {
        return (hs, he);
    }
    if let Some(p) = prefix_term
        && lower.starts_with(p)
    {
        let plen = p.chars().count() as u32;
        if plen < he - hs {
            return (hs, hs + plen);
        }
    }
    (hs, he)
}

/// Re-insert the sentinels into a mapped window — the §7.2 display form
/// (`the ⟦fog⟧ swallowing the ⟦ba⟧rn, keep this one`). Used by tests to
/// assert the normative artifact; the UI maps highlights to `<mark>`.
pub fn render_with_sentinels(text: &str, highlights: &[(u32, u32)]) -> String {
    let mut out = String::with_capacity(text.len() + highlights.len() * 6);
    let mut hl = highlights.iter().peekable();
    let mut open: Option<u32> = None;
    for (i, c) in text.chars().enumerate() {
        let i = i as u32;
        if let Some(&&(s, _)) = hl.peek()
            && s == i
        {
            let &(_, e) = hl.next().expect("peeked");
            out.push(OPEN);
            open = Some(e);
        }
        if open == Some(i) {
            out.push(CLOSE);
            open = None;
        }
        out.push(c);
    }
    if open.is_some() {
        out.push(CLOSE);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_text_window_with_prefix_trim() {
        let folded = "the fog swallowing the barn, keep this one";
        let snip = "the ⟦fog⟧ swallowing the ⟦barn⟧, keep this one";
        let m = map_snippet(folded, snip, &["fog".into()], Some("ba"));
        assert_eq!(m.text, folded);
        assert_eq!(
            (m.char_start, m.char_end),
            (0, folded.chars().count() as u32)
        );
        assert_eq!(m.highlights, vec![(4, 7), (23, 25)]);
        assert_eq!(
            render_with_sentinels(&m.text, &m.highlights),
            "the ⟦fog⟧ swallowing the ⟦ba⟧rn, keep this one"
        );
    }

    #[test]
    fn truncated_window_offsets_in_chars() {
        // Multi-byte chars before the window: offsets must count scalars.
        let folded = "Sebastião était là — the fog over the ridge at dawn, quiet and slow";
        let window = "the fog over the ridge";
        let snip = "…the ⟦fog⟧ over the ridge…";
        let m = map_snippet(folded, snip, &[], Some("fog"));
        assert_eq!(m.text, "the fog over the ridge");
        let start = folded
            .char_indices()
            .position(|(b, _)| folded[b..].starts_with(window))
            .unwrap() as u32;
        assert_eq!(m.char_start, start);
        assert_eq!(m.char_end, start + window.chars().count() as u32);
        assert_eq!(m.highlights, vec![(4, 7)]);
    }

    #[test]
    fn unmatched_token_keeps_whole_highlight() {
        // Diacritic-folded match: lowercase comparison cannot equate
        // "Sebastião" with "sebastiao" — conservative full-token highlight.
        let folded = "Sebastião at the harbor";
        let snip = "⟦Sebastião⟧ at the harbor";
        let m = map_snippet(folded, snip, &[], Some("sebastiao"));
        assert_eq!(m.highlights, vec![(0, 9)]);
    }

    #[test]
    fn unlocatable_window_falls_back_to_folded_text() {
        let folded = "completely different words";
        let m = map_snippet(folded, "…stale ⟦index⟧ row…", &[], None);
        assert_eq!(m.text, folded);
        assert_eq!((m.char_start, m.char_end), (0, 26));
        assert!(m.highlights.is_empty());
    }
}
