//! Finds the `async fn`s with a body in a Rust source file, for the coverage gate (ADR 0043).

/// Line numbers are 1-based, as in lcov.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncFn {
    /// The line of the `async` keyword.
    pub start: u32,
    /// The line of the `{` that opens the body.
    pub open: u32,
    /// The line of the `}` that closes the body.
    pub close: u32,
}

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Ident(String),
    Punct(char),
}

#[derive(Debug)]
struct Token {
    kind: Kind,
    line: u32,
}

impl Token {
    fn is_ident(&self, name: &str) -> bool {
        matches!(&self.kind, Kind::Ident(ident) if ident == name)
    }

    fn is_punct(&self, punct: char) -> bool {
        self.kind == Kind::Punct(punct)
    }
}

pub fn find(source: &str) -> Vec<AsyncFn> {
    let tokens = tokenize(source);
    let mut found = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if !token.is_ident("async") {
            continue;
        }
        let mut next = index + 1;
        for qualifier in ["unsafe", "extern"] {
            if tokens
                .get(next)
                .is_some_and(|token| token.is_ident(qualifier))
            {
                next += 1;
            }
        }
        if !tokens.get(next).is_some_and(|token| token.is_ident("fn")) {
            continue;
        }
        let Some(open) = body_open(&tokens, next + 1) else {
            continue;
        };
        let Some(close) = matching_close(&tokens, open) else {
            continue;
        };
        found.push(AsyncFn {
            start: token.line,
            open: tokens[open].line,
            close: tokens[close].line,
        });
    }
    found
}

/// The first `{` outside parentheses, brackets and generics, or `None` when a `;` (a
/// declaration without body) or the end of the file comes first.
fn body_open(tokens: &[Token], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(from) {
        let Kind::Punct(punct) = token.kind else {
            continue;
        };
        match punct {
            '{' if depth == 0 => return Some(index),
            ';' if depth == 0 => return None,
            '(' | '[' | '<' | '{' => depth += 1,
            '>' if tokens[index - 1].is_punct('-') => {}
            ')' | ']' | '>' | '}' => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    None
}

fn matching_close(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        if token.is_punct('{') {
            depth += 1;
        } else if token.is_punct('}') {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// Identifiers and punctuation; comments, string and char literals, lifetimes and
/// numbers are dropped.
fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let at = |index: usize| chars.get(index).copied();
    let mut tokens = Vec::new();
    let mut line = 1u32;
    let mut index = 0;
    while let Some(c) = at(index) {
        match c {
            '\n' => {
                line += 1;
                index += 1;
            }
            '/' if at(index + 1) == Some('/') => {
                while at(index).is_some_and(|c| c != '\n') {
                    index += 1;
                }
            }
            '/' if at(index + 1) == Some('*') => {
                let mut depth = 0usize;
                while let Some(c) = at(index) {
                    if c == '/' && at(index + 1) == Some('*') {
                        depth += 1;
                        index += 2;
                    } else if c == '*' && at(index + 1) == Some('/') {
                        depth -= 1;
                        index += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        line += u32::from(c == '\n');
                        index += 1;
                    }
                }
            }
            '"' => index = skip_string(&chars, index + 1, &mut line),
            '\'' => {
                index += if at(index + 1) == Some('\\') {
                    let mut end = index + 3;
                    while at(end).is_some_and(|c| c != '\'') {
                        end += 1;
                    }
                    end + 1 - index
                } else if at(index + 2) == Some('\'') {
                    3
                } else {
                    1
                };
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = index;
                while at(index).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    index += 1;
                }
                let ident: String = chars[start..index].iter().collect();
                match (ident.as_str(), at(index)) {
                    ("r" | "br" | "cr", Some('"' | '#')) => {
                        if let Some(end) = skip_raw_string(&chars, index, &mut line) {
                            index = end;
                            continue;
                        }
                    }
                    ("b" | "c", Some('"')) => {
                        index = skip_string(&chars, index + 1, &mut line);
                        continue;
                    }
                    _ => {}
                }
                tokens.push(Token {
                    kind: Kind::Ident(ident),
                    line,
                });
            }
            c if c.is_ascii_digit() => {
                while at(index).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    index += 1;
                }
            }
            c if c.is_whitespace() => index += 1,
            c => {
                tokens.push(Token {
                    kind: Kind::Punct(c),
                    line,
                });
                index += 1;
            }
        }
    }
    tokens
}

/// `index` is just past the opening quote; returns the index just past the closing one.
fn skip_string(chars: &[char], mut index: usize, line: &mut u32) -> usize {
    while let Some(&c) = chars.get(index) {
        match c {
            '"' => return index + 1,
            '\\' => {
                *line += u32::from(chars.get(index + 1) == Some(&'\n'));
                index += 2;
            }
            _ => {
                *line += u32::from(c == '\n');
                index += 1;
            }
        }
    }
    index
}

/// `index` is just past the `r`; returns `None` when this is a raw identifier, not a string.
fn skip_raw_string(chars: &[char], mut index: usize, line: &mut u32) -> Option<usize> {
    let mut hashes = 0;
    while chars.get(index) == Some(&'#') {
        hashes += 1;
        index += 1;
    }
    if chars.get(index) != Some(&'"') {
        return None;
    }
    index += 1;
    while let Some(&c) = chars.get(index) {
        index += 1;
        if c == '"'
            && chars.len() >= index + hashes
            && chars[index..index + hashes].iter().all(|&c| c == '#')
        {
            return Some(index + hashes);
        }
        *line += u32::from(c == '\n');
    }
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn spans(source: &str) -> Vec<(u32, u32, u32)> {
        find(source)
            .into_iter()
            .map(|f| (f.start, f.open, f.close))
            .collect()
    }

    #[test]
    fn one_line_signature() {
        let source = "async fn ping() -> u8 {\n    1\n}\n";
        assert_eq!(spans(source), [(1, 1, 3)]);
    }

    #[test]
    fn multi_line_parameters_generics_and_where_clause() {
        let source = r"
#[tracing::instrument(skip(pool))]
pub(crate) async fn load<T, const N: usize>(
    pool: &PgPool,
    ids: [u8; N],
    f: impl Fn(&str) -> Result<T, Error>,
) -> Result<Vec<Option<T>>, Error>
where
    T: Send + Into<Box<dyn std::error::Error>>,
{
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![])
}
";
        assert_eq!(spans(source), [(3, 10, 15)]);
    }

    #[test]
    fn trait_declarations_without_body_are_skipped() {
        let source = "
trait Store {
    async fn get(&self, id: u8) -> Option<u8>;
    async fn put(&self, id: u8) {
        let _ = id;
    }
}
";
        assert_eq!(spans(source), [(4, 4, 6)]);
    }

    #[test]
    fn methods_in_trait_impls_and_qualifiers() {
        let source = "
impl Store for Db {
    async fn get(&self, id: u8) -> Option<u8> { Some(id) }

    pub async unsafe fn raw(&self) {
        todo!()
    }

    async extern \"C\" fn ffi() {
    }
}
";
        assert_eq!(spans(source), [(3, 3, 3), (5, 5, 7), (9, 9, 10)]);
    }

    #[test]
    fn async_blocks_and_closures_are_not_functions() {
        let source = "
fn spawn() {
    let a = async move { 1 };
    let b = async || { 2 };
    let fn_name = 3;
}
";
        assert!(spans(source).is_empty());
    }

    #[test]
    fn nested_blocks_and_nested_async_fns() {
        let source = "
async fn outer() {
    async fn inner() {
        loop { break; }
    }
    match x { _ => {} }
}
";
        assert_eq!(spans(source), [(2, 2, 7), (3, 3, 5)]);
    }

    #[test]
    fn braces_in_comments_strings_and_chars_are_ignored() {
        let source = r##"
async fn tricky(
    // a { in a comment
    /* and } in /* a nested */ block
       comment */
    s: &'static str,
) {
    let a = "}";
    let b = '{';
    let c = '\'';
    let d = r#"} "# ;
    let e = b"{\"";
    let f = br#"{"#;
    let g = "multi
line }";
    let r#type = '\u{7d}';
    let h = 'h';
}
async fn after() {
}
"##;
        assert_eq!(spans(source), [(2, 7, 18), (19, 19, 20)]);
    }

    #[test]
    fn struct_patterns_in_parameters_are_not_the_body() {
        let source = "
async fn handler(
    State(AppState { pool, .. }): State<AppState>,
) -> Response {
    pool.into()
}
";
        assert_eq!(spans(source), [(2, 4, 6)]);
    }

    #[test]
    fn const_generic_block_in_signature() {
        let source = "async fn f() -> Foo<{ 1 + 1 }> {\n}\n";
        assert_eq!(spans(source), [(1, 1, 2)]);
    }

    #[test]
    fn unterminated_input_finds_nothing() {
        assert!(spans("async fn f(").is_empty());
        assert!(spans("async fn f() {").is_empty());
        assert!(spans("async fn f() -> u8").is_empty());
    }

    #[test]
    fn identifiers_containing_keywords_do_not_count() {
        let source = "fn not_async_fn() {}\nlet asyncfn = 1;\n";
        assert!(spans(source).is_empty());
    }

    proptest! {
        #[test]
        fn never_panics(source in "\\PC{0,200}") {
            let _ = find(&source);
        }

        #[test]
        fn finds_generated_functions_at_their_lines(
            blank_lines in proptest::collection::vec(0usize..4, 1..6),
            params in proptest::collection::vec(0usize..4, 1..6),
            body_lines in proptest::collection::vec(0usize..4, 1..6),
        ) {
            let mut source = String::new();
            let mut expected = Vec::new();
            let mut line = 1u32;
            for ((blank, params), body) in blank_lines.iter().zip(&params).zip(&body_lines) {
                for _ in 0..*blank {
                    source.push_str("// {\n");
                    line += 1;
                }
                let start = line;
                source.push_str("pub async fn f(\n");
                line += 1;
                for p in 0..*params {
                    source.push_str(&format!("    p{p}: Vec<&'static str>,\n"));
                    line += 1;
                }
                source.push_str(") -> Result<(), String> {\n");
                let open = line;
                line += 1;
                for _ in 0..*body {
                    source.push_str("    if x { let s = \"}\"; }\n");
                    line += 1;
                }
                source.push_str("}\n");
                expected.push((start, open, line));
                line += 1;
            }
            prop_assert_eq!(spans(&source), expected);
        }
    }
}
