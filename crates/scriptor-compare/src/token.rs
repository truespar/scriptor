//! Word-level tokenization with exact codepoint offsets.
//!
//! The inline diff runs over *words*, not characters - word granularity reads far better in a legal
//! redline (Word defaults to it too) and keeps the edit script small. Each token records its char
//! (codepoint) start/end so an op over tokens maps back to an exact `suggest_*` range: the model's
//! suggestion primitives address text by codepoint, so tokens do too.

/// A token: a maximal word, a maximal whitespace run, or a single other character (punctuation /
/// symbol). `start`/`end` are codepoint offsets into the source paragraph text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// Split `text` into word / whitespace / punctuation tokens. Concatenating the tokens' text
/// reproduces the input exactly (whitespace is preserved as its own tokens), so the mapping back to
/// codepoint ranges is lossless.
pub fn tokenize(text: &str) -> Vec<Token> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let c = chars[i];
        if is_word(c) {
            while i < chars.len() && is_word(chars[i]) {
                i += 1;
            }
        } else if c.is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        } else {
            // Punctuation / symbols each stand alone, so "may;" vs "may," diffs to the one char.
            i += 1;
        }
        tokens.push(Token { text: chars[start..i].iter().collect(), start, end: i });
    }
    tokens
}

/// A "word" character: alphanumeric plus the apostrophe that Word keeps inside a token ("Buyer's").
/// Splitting the apostrophe out would shatter possessives into noise. Hyphens stay separate so a
/// hyphenated edit ("arm's-length" -> "arms length") diffs to just the changed piece.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '\u{2019}'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless() {
        for s in ["", "hello world", "Party A shall pay $5,000.", "  leading\tand  trailing  "] {
            let joined: String = tokenize(s).iter().map(|t| t.text.as_str()).collect();
            assert_eq!(joined, s);
        }
    }

    #[test]
    fn offsets_are_codepoints() {
        // A multi-byte char before a word must not shift the word's codepoint offset.
        let toks = tokenize("\u{00e9}\u{00e9} word");
        let word = toks.iter().find(|t| t.text == "word").unwrap();
        assert_eq!(&"\u{00e9}\u{00e9} word".chars().collect::<Vec<_>>()[word.start..word.end]
            .iter().collect::<String>(), "word");
    }

    #[test]
    fn keeps_words_whole() {
        let toks: Vec<String> = tokenize("Buyer's arm's-length deal.").into_iter().map(|t| t.text).collect();
        assert!(toks.contains(&"Buyer's".to_string()));
        assert!(toks.contains(&"deal".to_string()));
    }
}
