//! Tokenize PureData message/atom strings into `Token` values.
//!
//! PD atoms are whitespace-separated. Special escapes:
//! - `\;`  → literal semicolon symbol
//! - `\,`  → literal comma symbol
//! - `\\`  → literal backslash
//! - `$0`  → DollarZero
//! - `$N`  → Dollar(N) for N ≥ 1
//!
//! Bare `,` in a message separates list items (within a single send).
//! Bare `;` terminates the record at the file level; it does NOT appear
//! inside a parsed token stream — the record splitter handles it first.

use crate::types::Token;

/// Parse a sequence of whitespace-separated PD atoms into tokens.
///
/// Bare `,` tokens are returned as `Token::Symbol(",")` so callers can split
/// on them to reconstruct list structure (used by `parse_message_content`).
pub fn tokenize(s: &str) -> Vec<Token> {
    s.split_whitespace().map(parse_atom).collect()
}

/// Parse a single PD atom string into a Token.
pub fn parse_atom(s: &str) -> Token {
    // Dollar substitution: $0, $1, $2, ...
    if let Some(rest) = s.strip_prefix('$') {
        if rest == "0" {
            return Token::DollarZero;
        }
        if let Ok(n) = rest.parse::<u32>() {
            return Token::Dollar(n);
        }
    }

    // Try numeric float
    if let Ok(f) = s.parse::<f64>() {
        return Token::Float(f);
    }

    // Symbol — resolve escape sequences
    Token::Symbol(unescape_symbol(s))
}

/// Resolve PD symbol escape sequences.
fn unescape_symbol(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(';') => {
                    out.push(';');
                    chars.next();
                }
                Some(',') => {
                    out.push(',');
                    chars.next();
                }
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                }
                _ => {
                    out.push('\\');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Escape a symbol string for emission back to a .pd file.
pub fn escape_symbol(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\\' => out.push_str("\\\\"),
            ' ' => out.push_str("\\ "), // spaces in symbols need escaping
            _ => out.push(c),
        }
    }
    out
}

/// Parse message box content into a list-of-lists structure.
///
/// Top-level messages are delimited by bare `;` tokens (which have been
/// removed by the record splitter — so content here is a single message).
/// Within that, `,` separates list branches.
///
/// Actually in PD, message boxes can contain multiple messages separated by
/// `;` within the message text — these are stored as `\;` in the file.
/// After unescape, a bare `;` in the atom stream means "message separator".
///
/// For simplicity, this function just tokenizes the content and splits on
/// comma-separator tokens.
pub fn parse_message_content(raw: &str) -> Vec<Vec<Token>> {
    // The raw string has already had the trailing `;` stripped by the record
    // splitter. Any `\;` inside are stored as `\\;` in the file and unescape
    // to `;` after unescape_symbol.
    //
    // Strategy: tokenize first, then split on Symbol(";") for messages and
    // Symbol(",") for list separators within a message.

    let tokens = tokenize(raw);

    // Split on semicolons into separate messages
    let messages_raw: Vec<Vec<Token>> = tokens
        .split(|t| matches!(t, Token::Symbol(s) if s == ";"))
        .filter(|m| !m.is_empty())
        .map(|m| m.to_vec())
        .collect();

    if messages_raw.is_empty() {
        // No semicolons — treat all tokens as a single flat message
        vec![tokens]
    } else {
        messages_raw
    }
}

/// Serialize a token back to a PD atom string.
pub fn emit_token(t: &Token) -> String {
    match t {
        Token::Float(f) => {
            if f.fract() == 0.0 && f.is_finite() {
                format!("{}", *f as i64)
            } else {
                format!("{f}")
            }
        }
        Token::Symbol(s) => escape_symbol(s),
        Token::Dollar(n) => format!("${n}"),
        Token::DollarZero => "$0".to_string(),
    }
}

/// Serialize a list of tokens back to a space-separated PD atom string.
pub fn emit_tokens(tokens: &[Token]) -> String {
    tokens.iter().map(emit_token).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_float() {
        assert_eq!(parse_atom("440"), Token::Float(440.0));
        assert_eq!(parse_atom("3.14"), Token::Float(3.14));
        assert_eq!(parse_atom("-1"), Token::Float(-1.0));
        assert_eq!(parse_atom("1e+037"), Token::Float(1e37));
    }

    #[test]
    fn test_dollar() {
        assert_eq!(parse_atom("$0"), Token::DollarZero);
        assert_eq!(parse_atom("$1"), Token::Dollar(1));
        assert_eq!(parse_atom("$99"), Token::Dollar(99));
    }

    #[test]
    fn test_symbol() {
        assert_eq!(parse_atom("hello"), Token::Symbol("hello".into()));
        assert_eq!(parse_atom("osc~"), Token::Symbol("osc~".into()));
    }

    #[test]
    fn test_escape() {
        assert_eq!(parse_atom("\\;"), Token::Symbol(";".into()));
        assert_eq!(parse_atom("\\,"), Token::Symbol(",".into()));
        assert_eq!(parse_atom("\\\\"), Token::Symbol("\\".into()));
    }

    #[test]
    fn test_roundtrip_symbol() {
        let s = "hello;world,test\\end";
        let escaped = escape_symbol(s);
        let tok = parse_atom(&escaped);
        assert_eq!(tok, Token::Symbol(s.into()));
    }

    #[test]
    fn test_tokenize_multiple() {
        let tokens = tokenize("440 osc~ $1");
        assert_eq!(
            tokens,
            vec![
                Token::Float(440.0),
                Token::Symbol("osc~".into()),
                Token::Dollar(1),
            ]
        );
    }
}
