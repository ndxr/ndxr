//! TF-IDF tokenizer for identifier-aware text splitting.
//!
//! Handles `camelCase`, `PascalCase`, `snake_case`, `SCREAMING_CASE`, and path
//! component splitting with stop-word removal. Used to build term-frequency
//! vectors for symbol search ranking.

use std::collections::{HashMap, HashSet};

/// Stop words to filter from tokenized output.
const STOP_WORDS: &[&str] = &[
    "a", "above", "after", "all", "an", "and", "any", "are", "as", "at", "be", "because", "been",
    "before", "being", "below", "between", "both", "but", "by", "can", "could", "did", "do",
    "does", "during", "each", "either", "every", "few", "for", "from", "had", "has", "have", "he",
    "her", "him", "his", "how", "i", "if", "in", "into", "is", "it", "its", "just", "may", "me",
    "might", "more", "most", "my", "neither", "no", "nor", "not", "of", "on", "only", "or",
    "other", "our", "own", "same", "shall", "she", "should", "so", "some", "such", "than", "that",
    "the", "their", "them", "these", "they", "this", "those", "through", "to", "too", "very",
    "was", "we", "were", "what", "when", "where", "which", "while", "who", "whom", "will", "with",
    "would", "yet", "you", "your",
];

/// Returns `true` if `word` is a stop word.
fn is_stop_word(word: &str) -> bool {
    STOP_WORDS.binary_search(&word).is_ok()
}

/// Splits an identifier into constituent terms.
///
/// Handles `camelCase`, `PascalCase`, `snake_case`, `SCREAMING_CASE`, and path
/// components separated by `/`, `::`, or `.`. All output is lowercased. Stop
/// words and single-character tokens are removed.
///
/// # Examples
///
/// ```
/// use ndxr::indexer::tokenizer::tokenize_identifier;
///
/// assert_eq!(tokenize_identifier("validateAuthToken"), vec!["validate", "auth", "token"]);
/// assert_eq!(tokenize_identifier("MAX_RETRIES"), vec!["max", "retries"]);
/// ```
#[must_use]
pub fn tokenize_identifier(name: &str) -> Vec<String> {
    let mut tokens = Vec::new();

    // 1. Split on path separators: /, ::, .
    //    Also strip file extensions (anything after the last dot in the last component).
    let parts = split_path_components(name);

    for part in parts {
        // 2. Split on underscores and hyphens.
        let subparts: Vec<&str> = part.split(['_', '-']).filter(|s| !s.is_empty()).collect();

        for subpart in subparts {
            // 3. Split on camelCase boundaries.
            let camel_parts = split_camel_case(subpart);
            for cp in camel_parts {
                let lower = cp.to_lowercase();
                if lower.len() > 1 && !is_stop_word(&lower) {
                    tokens.push(lower);
                }
            }
        }
    }

    tokens
}

/// Tokenizes a symbol's searchable text (name + docstring + FQN path components).
///
/// Combines tokens from the symbol name, its docstring (if present), and the
/// path components of its fully-qualified name (excluding the symbol name itself
/// to avoid duplication).
#[must_use]
pub fn tokenize_symbol(name: &str, docstring: Option<&str>, fqn: &str) -> Vec<String> {
    let mut tokens = tokenize_identifier(name);
    if let Some(doc) = docstring {
        tokens.extend(tokenize_text(doc));
    }
    // Add FQN path components (not the symbol name itself, already included).
    for part in fqn.split("::") {
        if part != name {
            tokens.extend(tokenize_identifier(part));
        }
    }
    tokens
}

/// Tokenizes free-form text (for docstrings and query strings).
///
/// Splits on whitespace and punctuation, lowercases everything, and removes
/// stop words and single-character tokens.
#[must_use]
pub fn tokenize_text(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || is_punctuation(c))
        .filter(|s| !s.is_empty())
        .flat_map(|word| {
            // Each word might still contain camelCase.
            split_camel_case(word)
                .into_iter()
                .map(|p| p.to_lowercase())
                .collect::<Vec<_>>()
        })
        .filter(|w| w.len() > 1 && !is_stop_word(w))
        .collect()
}

/// Computes term frequency vector: `tf(t) = count(t) / total_terms`.
///
/// Returns an empty map for empty input.
#[must_use]
#[allow(clippy::cast_precision_loss)] // usize->f64 loss negligible for counts
pub fn compute_tf(tokens: &[String]) -> HashMap<String, f64> {
    if tokens.is_empty() {
        return HashMap::new();
    }

    let mut counts: HashMap<&str, usize> = HashMap::with_capacity(tokens.len());
    for t in tokens {
        *counts.entry(t.as_str()).or_default() += 1;
    }

    let total = tokens.len() as f64;
    counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v as f64 / total))
        .collect()
}

/// Returns `true` if the character is punctuation for splitting purposes.
const fn is_punctuation(c: char) -> bool {
    matches!(
        c,
        ',' | ';'
            | ':'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | '"'
            | '\''
            | '`'
            | '!'
            | '?'
            | '#'
            | '$'
            | '%'
            | '^'
            | '&'
            | '*'
            | '+'
            | '='
            | '|'
            | '\\'
            | '~'
            | '@'
    )
}

/// Splits a name on path separators (`/`, `::`, `.`) and strips file extensions.
fn split_path_components(name: &str) -> Vec<String> {
    // Replace :: with / for uniform splitting, then split on / and .
    let normalized = name.replace("::", "/");
    let parts: Vec<&str> = normalized
        .split(['/', '.'])
        .filter(|s| !s.is_empty())
        .collect();

    // If the last part looks like a file extension (no uppercase, short), skip it.
    // Heuristic: if the original name contained a dot and the last segment is <= 4 chars
    // and all lowercase, treat it as a file extension.
    if name.contains('.')
        && parts.len() > 1
        && let Some(last) = parts.last()
        && last.len() <= 4
        && last.chars().all(|c| c.is_ascii_lowercase())
        && is_likely_extension(last)
    {
        return parts[..parts.len() - 1]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
    }

    parts.iter().map(|s| (*s).to_owned()).collect()
}

/// Returns `true` if the string looks like a common file extension.
const fn is_likely_extension(s: &str) -> bool {
    matches!(
        s.as_bytes(),
        b"ts"
            | b"tsx"
            | b"js"
            | b"jsx"
            | b"mjs"
            | b"cjs"
            | b"py"
            | b"pyi"
            | b"go"
            | b"rs"
            | b"java"
            | b"cs"
            | b"rb"
            | b"sh"
            | b"bash"
            | b"php"
            | b"zig"
            | b"c"
            | b"h"
            | b"cpp"
            | b"cc"
            | b"cxx"
            | b"hpp"
            | b"hh"
            | b"hxx"
            | b"json"
            | b"yaml"
            | b"yml"
            | b"toml"
            | b"xml"
            | b"html"
            | b"css"
            | b"md"
            | b"txt"
            | b"cfg"
            | b"ini"
            | b"log"
    )
}

/// Splits a string on `camelCase` / `PascalCase` boundaries.
///
/// Examples:
/// - `"validateAuthToken"` -> `["validate", "Auth", "Token"]`
/// - `"HTTPSClient"` -> `["HTTPS", "Client"]`
/// - `"MAX"` -> `["MAX"]`
fn split_camel_case(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut prev_char: Option<char> = None;
    let mut iter = s.chars().peekable();

    while let Some(c) = iter.next() {
        if current.is_empty() {
            current.push(c);
            prev_char = Some(c);
            continue;
        }

        let prev_upper = prev_char.is_some_and(|p| p.is_ascii_uppercase());
        let curr_upper = c.is_ascii_uppercase();
        let next_lower = iter.peek().is_some_and(char::is_ascii_lowercase);

        // Split on lowercase -> uppercase transition (camelCase boundary).
        if !prev_upper && curr_upper {
            parts.push(std::mem::take(&mut current));
            current.push(c);
            prev_char = Some(c);
            continue;
        }

        // Split when transitioning from UPPER run to new word:
        // "HTTPSClient" -> "HTTPS" + "Client".
        if prev_upper && curr_upper && next_lower && current.len() > 1 {
            // Move the last char of current to the new segment.
            let last = current.pop().unwrap_or_default();
            parts.push(std::mem::take(&mut current));
            current.push(last);
            current.push(c);
            prev_char = Some(c);
            continue;
        }

        current.push(c);
        prev_char = Some(c);
    }

    if !current.is_empty() {
        parts.push(current);
    }

    parts
}

/// Characters that have special meaning in FTS5 MATCH expressions.
const FTS_SPECIAL: &[char] = &[
    '"', '\'', '(', ')', '{', '}', '[', ']', '*', ':', '^', '-', '+', '~', '|', '&', '.', ',', ';',
    '!', '?', '@', '#', '$', '%', '\\', '/', '=', '`', '<', '>',
];

/// Returns `true` if a character is special in FTS5.
#[must_use]
pub fn is_fts_special(c: char) -> bool {
    FTS_SPECIAL.contains(&c)
}

/// Builds a sanitized FTS5 MATCH query from a raw search string.
///
/// Strips FTS5 special characters, splits into words, filters empty tokens,
/// and joins with OR.
#[must_use]
pub fn build_fts_query(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| if is_fts_special(c) { ' ' } else { c })
        .collect();
    let terms: Vec<&str> = sanitized
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return String::new();
    }
    terms
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Extracts character trigrams from a string.
///
/// Returns an empty set for strings shorter than 3 characters.
/// Input is lowercased before extraction.
fn extract_trigrams(s: &str) -> HashSet<String> {
    let lower = s.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    if chars.len() < 3 {
        return HashSet::new();
    }
    let mut trigrams = HashSet::with_capacity(chars.len() - 2);
    for window in chars.windows(3) {
        trigrams.insert(window.iter().collect());
    }
    trigrams
}

/// Extracts trigrams from an identifier by splitting into parts first.
///
/// Splits `camelCase`/`snake_case` via `tokenize_identifier`, extracts trigrams
/// from each part, and unions them with trigrams of the raw lowercased string.
fn identifier_trigrams(s: &str) -> HashSet<String> {
    let parts = tokenize_identifier(s);
    let mut all_trigrams = HashSet::new();
    for part in &parts {
        all_trigrams.extend(extract_trigrams(part));
    }
    all_trigrams.extend(extract_trigrams(s));
    all_trigrams
}

/// Computes trigram Jaccard similarity between two identifier strings.
///
/// Both strings are split into `camelCase`/`snake_case` parts, trigrams are
/// extracted from each part, and the union of all trigrams is used for
/// Jaccard computation: `|intersection| / |union|`.
///
/// Returns a value in \[0.0, 1.0\]. Returns 0.0 if either string produces
/// no trigrams (e.g., strings shorter than 3 characters).
#[must_use]
pub fn trigram_similarity(a: &str, b: &str) -> f64 {
    let trigrams_a = identifier_trigrams(a);
    let trigrams_b = identifier_trigrams(b);
    if trigrams_a.is_empty() || trigrams_b.is_empty() {
        return 0.0;
    }
    let intersection = trigrams_a.intersection(&trigrams_b).count();
    let union = trigrams_a.union(&trigrams_b).count();
    if union == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)] // set sizes are small
    let result = intersection as f64 / union as f64;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_case_splitting() {
        assert_eq!(
            tokenize_identifier("validateAuthToken"),
            vec!["validate", "auth", "token"]
        );
    }

    #[test]
    fn snake_case_splitting() {
        assert_eq!(
            tokenize_identifier("validate_auth_token"),
            vec!["validate", "auth", "token"]
        );
    }

    #[test]
    fn screaming_case_splitting() {
        assert_eq!(tokenize_identifier("MAX_RETRIES"), vec!["max", "retries"]);
    }

    #[test]
    fn path_component_splitting() {
        assert_eq!(
            tokenize_identifier("src/auth/middleware.ts"),
            vec!["src", "auth", "middleware"]
        );
    }

    #[test]
    fn stop_words_removed() {
        // "the" and "a" and "is" should be removed.
        let tokens = tokenize_text("the quick fox is a fast runner");
        assert!(!tokens.contains(&"the".to_owned()));
        assert!(!tokens.contains(&"a".to_owned()));
        assert!(!tokens.contains(&"is".to_owned()));
        assert!(tokens.contains(&"quick".to_owned()));
        assert!(tokens.contains(&"fox".to_owned()));
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(tokenize_identifier("").is_empty());
        assert!(tokenize_text("").is_empty());
    }

    #[test]
    fn compute_tf_frequencies() {
        let tokens = vec![
            "foo".to_owned(),
            "bar".to_owned(),
            "foo".to_owned(),
            "baz".to_owned(),
        ];
        let tf = compute_tf(&tokens);
        let epsilon = f64::EPSILON;
        assert!((tf["foo"] - 0.5).abs() < epsilon);
        assert!((tf["bar"] - 0.25).abs() < epsilon);
        assert!((tf["baz"] - 0.25).abs() < epsilon);
    }

    #[test]
    fn compute_tf_empty() {
        let tf = compute_tf(&[]);
        assert!(tf.is_empty());
    }

    #[test]
    fn single_char_tokens_removed() {
        // Single-char tokens like "x" should be removed.
        assert!(tokenize_identifier("x").is_empty());
    }

    #[test]
    fn pascal_case_splitting() {
        assert_eq!(tokenize_identifier("AuthService"), vec!["auth", "service"]);
    }

    #[test]
    fn double_colon_path_splitting() {
        assert_eq!(
            tokenize_identifier("std::collections::HashMap"),
            vec!["std", "collections", "hash", "map"]
        );
    }

    #[test]
    fn stop_words_are_sorted() {
        for window in STOP_WORDS.windows(2) {
            assert!(
                window[0] < window[1],
                "STOP_WORDS not sorted: {:?} >= {:?}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn trigram_similarity_identical() {
        assert!((trigram_similarity("auth", "auth") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn trigram_similarity_prefix_match() {
        let sim = trigram_similarity("auth", "authenticate");
        assert!(
            sim > 0.0,
            "prefix should have non-zero similarity, got {sim}"
        );
        assert!(sim < 1.0, "prefix should not be identical, got {sim}");
    }

    #[test]
    fn trigram_similarity_no_overlap() {
        assert!(
            trigram_similarity("xyz", "authenticate").abs() < f64::EPSILON,
            "no shared trigrams should give zero similarity"
        );
    }

    #[test]
    fn trigram_similarity_short_strings() {
        assert!(
            trigram_similarity("ab", "authenticate").abs() < f64::EPSILON,
            "strings shorter than 3 chars produce no trigrams"
        );
    }

    #[test]
    fn trigram_similarity_case_insensitive() {
        assert!(
            (trigram_similarity("Auth", "auth") - 1.0).abs() < f64::EPSILON,
            "should be case-insensitive"
        );
    }

    #[test]
    fn trigram_similarity_snake_case_split() {
        let sim = trigram_similarity("validate", "validate_token");
        assert!(sim > 0.3, "snake_case parts should match, got {sim}");
    }

    #[test]
    fn trigram_similarity_camel_case_split() {
        let sim = trigram_similarity("auth", "AuthService");
        assert!(sim > 0.2, "camelCase parts should match, got {sim}");
    }
}
