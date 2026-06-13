//! Word-error-rate scoring for the voice-pipeline stress harness
//! (BACKLOG "Audiobook WER stress harness", founder, June 2026).
//!
//! WHY this lives in the core lib and not inline in `pp_voice_bench`:
//! the founder's headline question is *separating MODEL accuracy from
//! PIPELINE truncation* — score the RAW (ungated) feed and the GATED
//! (production VAD) feed against the SAME reference transcript and read
//! the gap. That comparison is only trustworthy if the scorer is itself
//! tested, so the algorithm sits here behind unit tests rather than in a
//! `main()` no machine ever exercises in CI.
//!
//! The metric is the standard ASR WER: align the hypothesis words to the
//! reference words by minimum word-level edit distance (Levenshtein over
//! WORDS, not characters), then
//!
//!   WER = (S + D + I) / N
//!
//! where S/D/I are substitutions/deletions/insertions on the optimal
//! alignment path and N is the reference word count. `hits` (correct
//! words) and a `hit_rate` are reported alongside because on a long
//! audiobook chapter the raw S/D/I/N counts are what actually localize a
//! regression — a single WER float hides whether the model is mishearing
//! words (S) or the pipeline is dropping them (D).

/// The result of scoring one hypothesis against one reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WerScore {
    /// Word error rate = (sub + del + ins) / ref_words. 0.0 = perfect.
    /// When the reference is empty this is 0.0 if the hypothesis is also
    /// empty, else `ins as f64` (every hypothesis word is an insertion —
    /// there is no N to divide by, so we report the raw inserted count as
    /// the rate, which is the convention `jiwer`/`sclite` degenerate to).
    pub wer: f64,
    /// Substitutions on the optimal alignment.
    pub sub: usize,
    /// Deletions (reference words with no hypothesis match).
    pub del: usize,
    /// Insertions (hypothesis words with no reference match).
    pub ins: usize,
    /// Reference word count (the N in the denominator).
    pub ref_words: usize,
    /// Hypothesis word count (reported for sanity-checking truncation).
    pub hyp_words: usize,
    /// Correctly aligned words (ref_words - sub - del).
    pub hits: usize,
}

impl WerScore {
    /// Fraction of reference words correctly transcribed (hits / N).
    /// Complements `wer`: on an empty reference this is 1.0 (nothing to
    /// get wrong) so a clean run reads 100% regardless of which metric
    /// the caller prints.
    pub fn hit_rate(&self) -> f64 {
        if self.ref_words == 0 {
            1.0
        } else {
            self.hits as f64 / self.ref_words as f64
        }
    }
}

/// Normalize text for word-level comparison, returning the token stream.
///
/// The reference (Project Gutenberg) and the hypothesis (ASR output)
/// disagree on surface form, never on words, so we fold everything that
/// is not a word before aligning:
///   - lowercase (ASR is caseless; Gutenberg is Title/Sentence case);
///   - strip punctuation and symbols (the ASR emits none; Gutenberg is
///     full of it) — any char that is not alphanumeric becomes a break;
///   - collapse whitespace, so chapter line-wrapping in the transcript
///     does not invent token boundaries.
///
/// Apostrophes and hyphens are treated as breaks too: the ASR writes
/// "dont"/"well behaved" forms inconsistently, so splitting on them keeps
/// "don't" vs "dont" from scoring as a spurious substitution. This is the
/// README caveat ("transcript carries Gutenberg punctuation the model
/// never voices") made concrete — we compare WORDS, letters and digits
/// only.
pub fn normalize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// Score a hypothesis transcript against a reference transcript.
///
/// Both strings are normalized (see [`normalize`]) and aligned by
/// word-level Levenshtein distance. The DP fills a (N+1)×(M+1) cost
/// table, then back-traces the optimal path to attribute each edit to
/// substitution / deletion / insertion — the counts, not just the total
/// distance, are what the harness reports.
pub fn score(reference: &str, hypothesis: &str) -> WerScore {
    let r = normalize(reference);
    let h = normalize(hypothesis);
    score_tokens(&r, &h)
}

/// Core alignment over already-normalized token slices. Split out so the
/// DP is testable directly and so the bin can pass pre-tokenized words.
pub fn score_tokens(r: &[String], h: &[String]) -> WerScore {
    let n = r.len();
    let m = h.len();

    // Degenerate references: no N to divide by. An empty hypothesis
    // against a non-empty reference is the canonical "WER = 1.0" case
    // (all N words deleted); the empty/empty case is a perfect 0.0.
    if n == 0 {
        return WerScore {
            wer: if m == 0 { 0.0 } else { m as f64 },
            sub: 0,
            del: 0,
            ins: m,
            ref_words: 0,
            hyp_words: m,
            hits: 0,
        };
    }

    // dp[i][j] = min edits aligning r[..i] to h[..j]. Row-major flat
    // vector to keep one allocation; (m+1) columns per row.
    let cols = m + 1;
    let mut dp = vec![0u32; (n + 1) * cols];
    let idx = |i: usize, j: usize| i * cols + j;
    // First row/column: aligning against nothing is pure ins/del.
    for j in 0..=m {
        dp[idx(0, j)] = j as u32;
    }
    for i in 0..=n {
        dp[idx(i, 0)] = i as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            // Substitution cost is 0 on a word match, else 1.
            let subst = dp[idx(i - 1, j - 1)] + u32::from(r[i - 1] != h[j - 1]);
            let del = dp[idx(i - 1, j)] + 1; // skip a reference word
            let ins = dp[idx(i, j - 1)] + 1; // skip a hypothesis word
            dp[idx(i, j)] = subst.min(del).min(ins);
        }
    }

    // Back-trace from (n, m) to (0, 0) to split the total distance into
    // S/D/I. Ties are broken toward the diagonal (match/sub) so that a
    // word that could read as "sub" or "del+ins" is counted as one sub —
    // matching sclite/jiwer convention and keeping the counts honest.
    let (mut i, mut j) = (n, m);
    let (mut sub, mut del, mut ins, mut hits) = (0usize, 0usize, 0usize, 0usize);
    while i > 0 || j > 0 {
        let here = dp[idx(i, j)];
        if i > 0 && j > 0 {
            let matched = r[i - 1] == h[j - 1];
            let diag = dp[idx(i - 1, j - 1)];
            if diag + u32::from(!matched) == here {
                if matched {
                    hits += 1;
                } else {
                    sub += 1;
                }
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && dp[idx(i - 1, j)] + 1 == here {
            del += 1; // a reference word with no hypothesis partner
            i -= 1;
            continue;
        }
        // Remaining case: insertion (advance only j).
        ins += 1;
        j -= 1;
    }

    WerScore {
        wer: (sub + del + ins) as f64 / n as f64,
        sub,
        del,
        ins,
        ref_words: n,
        hyp_words: m,
        hits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    #[test]
    fn identical_strings_score_zero() {
        let r = score("the quick brown fox", "the quick brown fox");
        assert_eq!(r.wer, 0.0);
        assert_eq!((r.sub, r.del, r.ins), (0, 0, 0));
        assert_eq!(r.ref_words, 4);
        assert_eq!(r.hits, 4);
        assert_eq!(r.hit_rate(), 1.0);
    }

    #[test]
    fn one_substitution() {
        // "brown" -> "black": one sub, N = 4, WER = 0.25.
        let r = score("the quick brown fox", "the quick black fox");
        assert_eq!((r.sub, r.del, r.ins), (1, 0, 0));
        assert_eq!(r.ref_words, 4);
        assert_eq!(r.hits, 3);
        assert!((r.wer - 0.25).abs() < 1e-9);
    }

    #[test]
    fn one_deletion() {
        // Hypothesis dropped "brown": one del, N = 4, WER = 0.25.
        let r = score("the quick brown fox", "the quick fox");
        assert_eq!((r.sub, r.del, r.ins), (0, 1, 0));
        assert_eq!(r.ref_words, 4);
        assert_eq!(r.hits, 3);
        assert!((r.wer - 0.25).abs() < 1e-9);
    }

    #[test]
    fn one_insertion() {
        // Hypothesis added "lazy": one ins, N = 4, WER = 0.25.
        let r = score("the quick brown fox", "the quick brown lazy fox");
        assert_eq!((r.sub, r.del, r.ins), (0, 0, 1));
        assert_eq!(r.ref_words, 4);
        assert_eq!(r.hyp_words, 5);
        assert_eq!(r.hits, 4);
        assert!((r.wer - 0.25).abs() < 1e-9);
    }

    #[test]
    fn empty_hypothesis_is_total_loss() {
        // Every reference word deleted: WER = 1.0 (the truncation floor).
        let r = score("the quick brown fox", "");
        assert_eq!((r.sub, r.del, r.ins), (0, 4, 0));
        assert_eq!(r.ref_words, 4);
        assert_eq!(r.hyp_words, 0);
        assert_eq!(r.hits, 0);
        assert_eq!(r.wer, 1.0);
        assert_eq!(r.hit_rate(), 0.0);
    }

    #[test]
    fn empty_reference_counts_insertions() {
        // No N to divide by: empty/empty is perfect, else raw ins count.
        let empty = score("", "");
        assert_eq!(empty.wer, 0.0);
        assert_eq!(empty.hit_rate(), 1.0);

        let spurious = score("", "hello world");
        assert_eq!((spurious.sub, spurious.del, spurious.ins), (0, 0, 2));
        assert_eq!(spurious.wer, 2.0);
    }

    #[test]
    fn normalization_folds_case_and_punctuation() {
        // Same words, different surface form -> perfect score. This is
        // the whole point: Gutenberg punctuation/case must not register
        // as errors against the caseless, punctuation-free ASR output.
        let r = score("Hello, World! It's me.", "hello world it s me");
        assert_eq!(r.wer, 0.0, "punctuation/case must normalize away");
        assert_eq!(r.ref_words, 5);
    }

    #[test]
    fn normalize_splits_on_non_alphanumeric() {
        assert_eq!(
            normalize("Don't  stop\nnow!"),
            s(&["don", "t", "stop", "now"])
        );
        assert_eq!(normalize("  "), Vec::<String>::new());
        assert_eq!(normalize("well-behaved"), s(&["well", "behaved"]));
    }

    #[test]
    fn mixed_edits_sum_correctly() {
        // ref: a b c d e  (N = 5)
        // hyp: a x c e f  -> sub(b->x), del(d), sub? let the DP decide.
        // We assert the invariant rather than a hand-traced path: the
        // total edit count must equal S + D + I and WER = that / N.
        let r = score("a b c d e", "a x c e f");
        assert_eq!(r.ref_words, 5);
        assert_eq!(r.sub + r.del, 5 - r.hits);
        let expected = (r.sub + r.del + r.ins) as f64 / 5.0;
        assert!((r.wer - expected).abs() < 1e-9);
    }

    #[test]
    fn score_tokens_matches_score() {
        let viatext = score("the quick brown fox", "the quick black fox");
        let viatokens = score_tokens(
            &s(&["the", "quick", "brown", "fox"]),
            &s(&["the", "quick", "black", "fox"]),
        );
        assert_eq!(viatext, viatokens);
    }
}
