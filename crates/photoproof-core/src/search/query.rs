//! §4 query construction (normative): whitespace split, drop empty tokens,
//! escape each token by doubling internal `"`, all tokens but the last as
//! quoted exact terms, the last as a prefix term.
//!
//! Implicit AND (FTS5 default). No user-facing boolean syntax in v1: `OR`,
//! `NEAR`, `-` are plain tokens — quoting neutralizes them.

/// Build the FTS5 MATCH string for a raw input. `None` when the input has no
/// tokens (the filter-only browse path).
///
/// ```
/// use photoproof_core::search::fts_match_query;
/// assert_eq!(fts_match_query("melanch").as_deref(), Some("\"melanch\"*"));
/// assert_eq!(fts_match_query("fog barn").as_deref(), Some("\"fog\" \"barn\"*"));
/// assert_eq!(fts_match_query("  \t "), None);
/// ```
pub fn fts_match_query(input: &str) -> Option<String> {
    let tokens: Vec<&str> = input.split_whitespace().collect();
    let (last, exact) = tokens.split_last()?;
    let mut out = String::with_capacity(input.len() + tokens.len() * 3 + 1);
    for tok in exact {
        push_quoted(&mut out, tok);
        out.push(' ');
    }
    push_quoted(&mut out, last);
    out.push('*');
    Some(out)
}

/// Append `"tok"` with internal `"` doubled.
fn push_quoted(out: &mut String, tok: &str) {
    out.push('"');
    for c in tok.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
}

/// The query tokens as the highlight mapper needs them: all-but-last exact
/// terms and the last prefix term, lowercased (case-insensitive matching;
/// the tokenizer's diacritic folding is approximated by exact/lowercase
/// comparison — unmatched tokens keep their full-token highlight).
pub(crate) fn highlight_terms(input: &str) -> (Vec<String>, Option<String>) {
    let tokens: Vec<&str> = input.split_whitespace().collect();
    match tokens.split_last() {
        None => (Vec::new(), None),
        Some((last, exact)) => (
            exact.iter().map(|t| t.to_lowercase()).collect(),
            Some(last.to_lowercase()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_matches_spec_examples() {
        // §4 worked examples, verbatim.
        assert_eq!(fts_match_query("melanch").as_deref(), Some("\"melanch\"*"));
        assert_eq!(
            fts_match_query("fog barn").as_deref(),
            Some("\"fog\" \"barn\"*")
        );
        // §7.2: typing `fog ba`.
        assert_eq!(
            fts_match_query("fog ba").as_deref(),
            Some("\"fog\" \"ba\"*")
        );
    }

    #[test]
    fn empties_dropped_and_whitespace_only_is_none() {
        assert_eq!(fts_match_query(""), None);
        assert_eq!(fts_match_query("   \t\n "), None);
        assert_eq!(
            fts_match_query("  fog\t\tba  ").as_deref(),
            Some("\"fog\" \"ba\"*")
        );
    }

    #[test]
    fn internal_quotes_doubled() {
        assert_eq!(
            fts_match_query("fo\"g ba").as_deref(),
            Some("\"fo\"\"g\" \"ba\"*")
        );
        assert_eq!(fts_match_query("\"").as_deref(), Some("\"\"\"\"*"));
    }

    #[test]
    fn boolean_syntax_neutralized_by_quoting() {
        // OR / NEAR / - are plain tokens (§4).
        assert_eq!(
            fts_match_query("fog OR barn").as_deref(),
            Some("\"fog\" \"OR\" \"barn\"*")
        );
        assert_eq!(
            fts_match_query("NEAR -fog").as_deref(),
            Some("\"NEAR\" \"-fog\"*")
        );
    }
}
