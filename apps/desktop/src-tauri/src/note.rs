//! Typed-pipeline text rule (CAPTURE §4): stored byte-for-byte verbatim; the
//! app MAY trim exactly one trailing newline, nothing else; empty or
//! whitespace-only submissions mint nothing.

pub fn normalize_note(input: &str) -> Option<String> {
    if input.trim().is_empty() {
        return None;
    }
    let trimmed = input
        .strip_suffix("\r\n")
        .or_else(|| input.strip_suffix('\n'))
        .unwrap_or(input);
    Some(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::normalize_note;

    #[test]
    fn whitespace_only_mints_nothing() {
        assert_eq!(normalize_note(""), None);
        assert_eq!(normalize_note("   \n\t  "), None);
        assert_eq!(normalize_note("\n"), None);
    }

    #[test]
    fn trims_exactly_one_trailing_newline_nothing_else() {
        assert_eq!(normalize_note("note\n"), Some("note".into()));
        assert_eq!(normalize_note("note\r\n"), Some("note".into()));
        // Only ONE trailing newline; the second survives verbatim.
        assert_eq!(normalize_note("note\n\n"), Some("note\n".into()));
        // Interior structure and surrounding spaces are untouched.
        assert_eq!(
            normalize_note("  two\nlines  "),
            Some("  two\nlines  ".into())
        );
        // No markdown or other processing — bytes through verbatim.
        assert_eq!(
            normalize_note("**not markdown** <not html>"),
            Some("**not markdown** <not html>".into())
        );
    }
}
