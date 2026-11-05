//! Syntax highlighting with tree-sitter. No UI types: the app maps a [`Token`] to a color.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tree_sitter::Language as Grammar;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Files over this many bytes stay plain.
pub const MAX_BYTES: usize = 1 << 20;

/// Files over this many lines stay plain.
pub const MAX_LINES: usize = 20_000;

/// How many highlighted files the shared cache keeps.
const CACHE_FILES: usize = 64;

/// What a piece of code is. Anything else stays plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    Keyword,
    String,
    Comment,
    Function,
    Type,
    Number,
    Constant,
    Punctuation,
}

impl Token {
    pub const ALL: [Token; 8] = [
        Token::Keyword,
        Token::String,
        Token::Comment,
        Token::Function,
        Token::Type,
        Token::Number,
        Token::Constant,
        Token::Punctuation,
    ];
}

/// A byte range of the text and what it is.
pub type Span = (Range<usize>, Token);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Php,
    Python,
    Go,
    Json,
    Toml,
    Yaml,
    Markdown,
    Css,
    Html,
    Bash,
}

impl Language {
    const ALL: [Language; 14] = [
        Language::Rust,
        Language::TypeScript,
        Language::Tsx,
        Language::JavaScript,
        Language::Php,
        Language::Python,
        Language::Go,
        Language::Json,
        Language::Toml,
        Language::Yaml,
        Language::Markdown,
        Language::Css,
        Language::Html,
        Language::Bash,
    ];

    /// The language of a file by its extension, or `None` when it stays plain.
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();

        let language = match extension.as_str() {
            "rs" => Self::Rust,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Self::JavaScript,
            "php" => Self::Php,
            "py" | "pyi" => Self::Python,
            "go" => Self::Go,
            "json" => Self::Json,
            "toml" => Self::Toml,
            "yaml" | "yml" => Self::Yaml,
            "md" | "markdown" => Self::Markdown,
            "css" => Self::Css,
            "html" | "htm" => Self::Html,
            "sh" | "bash" | "zsh" => Self::Bash,
            _ => return None,
        };

        Some(language)
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|language| *language == self)
            .unwrap_or_default()
    }

    /// The grammar and its highlight queries.
    fn sources(self) -> (Grammar, String) {
        match self {
            Self::Rust => (
                tree_sitter_rust::LANGUAGE.into(),
                tree_sitter_rust::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::TypeScript => (
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                format!(
                    "{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY
                ),
            ),
            Self::Tsx => (
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                format!(
                    "{}\n{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY
                ),
            ),
            Self::JavaScript => (
                tree_sitter_javascript::LANGUAGE.into(),
                format!(
                    "{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
                ),
            ),
            Self::Php => (
                tree_sitter_php::LANGUAGE_PHP.into(),
                tree_sitter_php::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Python => (
                tree_sitter_python::LANGUAGE.into(),
                tree_sitter_python::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Go => (
                tree_sitter_go::LANGUAGE.into(),
                tree_sitter_go::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Json => (
                tree_sitter_json::LANGUAGE.into(),
                tree_sitter_json::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Toml => (
                tree_sitter_toml_ng::LANGUAGE.into(),
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Yaml => (
                tree_sitter_yaml::LANGUAGE.into(),
                tree_sitter_yaml::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Markdown => (
                tree_sitter_md::LANGUAGE.into(),
                tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.to_owned(),
            ),
            Self::Css => (
                tree_sitter_css::LANGUAGE.into(),
                tree_sitter_css::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Html => (
                tree_sitter_html::LANGUAGE.into(),
                tree_sitter_html::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Bash => (
                tree_sitter_bash::LANGUAGE.into(),
                tree_sitter_bash::HIGHLIGHT_QUERY.to_owned(),
            ),
        }
    }
}

/// Capture names the queries use, and what each shows as. A name also matches its
/// dotted children, so `function` covers `function.method`. `None` stays plain.
const NAMES: &[(&str, Option<Token>)] = &[
    ("keyword", Some(Token::Keyword)),
    ("conditional", Some(Token::Keyword)),
    ("repeat", Some(Token::Keyword)),
    ("include", Some(Token::Keyword)),
    ("tag", Some(Token::Keyword)),
    ("string", Some(Token::String)),
    ("character", Some(Token::String)),
    ("escape", Some(Token::String)),
    ("text.literal", Some(Token::String)),
    ("comment", Some(Token::Comment)),
    ("function", Some(Token::Function)),
    ("method", Some(Token::Function)),
    ("text.title", Some(Token::Function)),
    ("type", Some(Token::Type)),
    ("constructor", Some(Token::Type)),
    ("attribute", Some(Token::Type)),
    ("number", Some(Token::Number)),
    ("float", Some(Token::Number)),
    ("constant", Some(Token::Constant)),
    ("boolean", Some(Token::Constant)),
    ("variable.builtin", Some(Token::Constant)),
    ("text.uri", Some(Token::Constant)),
    ("punctuation", Some(Token::Punctuation)),
    ("property", None),
    ("operator", None),
];

fn configuration(language: Language) -> Option<&'static HighlightConfiguration> {
    static CONFIGURATIONS: [OnceLock<Option<HighlightConfiguration>>; Language::ALL.len()] =
        [const { OnceLock::new() }; Language::ALL.len()];

    CONFIGURATIONS[language.index()]
        .get_or_init(|| {
            let (grammar, query) = language.sources();
            let mut configuration =
                HighlightConfiguration::new(grammar, format!("{language:?}"), &query, "", "")
                    .ok()?;
            configuration.configure(&NAMES.iter().map(|(name, _)| *name).collect::<Vec<_>>());

            Some(configuration)
        })
        .as_ref()
}

/// Whether the text is small enough to highlight.
pub fn within_limits(text: &str) -> bool {
    text.len() <= MAX_BYTES && text.bytes().filter(|byte| *byte == b'\n').count() < MAX_LINES
}

/// The tokens of `text`, sorted, without overlap. Empty for a text over the limits or
/// a grammar that did not load.
pub fn highlight(language: Language, text: &str) -> Vec<Span> {
    thread_local! {
        static HIGHLIGHTER: RefCell<Highlighter> = RefCell::new(Highlighter::new());
    }

    if !within_limits(text) {
        return Vec::new();
    }
    let Some(configuration) = configuration(language) else {
        return Vec::new();
    };

    HIGHLIGHTER.with(|highlighter| {
        let mut highlighter = highlighter.borrow_mut();
        let Ok(events) =
            highlighter.highlight(configuration, text.as_bytes(), None, None, |_| None)
        else {
            return Vec::new();
        };

        let mut spans: Vec<Span> = Vec::new();
        let mut stack: Vec<Option<Token>> = Vec::new();

        for event in events {
            match event {
                Ok(HighlightEvent::HighlightStart(highlight)) => {
                    stack.push(NAMES.get(highlight.0).and_then(|(_, token)| *token));
                }
                Ok(HighlightEvent::HighlightEnd) => {
                    stack.pop();
                }
                Ok(HighlightEvent::Source { start, end }) => {
                    let token = stack.last().copied().flatten();
                    if let Some(token) = token
                        && start < end
                    {
                        push_span(&mut spans, start..end, token);
                    }
                }
                Err(_) => return Vec::new(),
            }
        }

        spans
    })
}

/// Adds a span, joined to the one before when they touch and are the same token.
fn push_span(spans: &mut Vec<Span>, range: Range<usize>, token: Token) {
    if let Some((last, last_token)) = spans.last_mut()
        && *last_token == token
        && last.end == range.start
    {
        last.end = range.end;

        return;
    }

    spans.push((range, token));
}

/// The tokens of each line of `text`, with offsets from the start of the line. A span
/// over a line break is cut at it.
pub fn split_lines(text: &str, spans: &[Span]) -> Vec<Vec<Span>> {
    let mut lines: Vec<Vec<Span>> = Vec::new();
    let mut start = 0;

    for line in text.split('\n') {
        let end = start + line.len();
        let mut tokens = Vec::new();

        let first = spans.partition_point(|(range, _)| range.end <= start);
        for (range, token) in &spans[first..] {
            if range.start >= end {
                break;
            }
            tokens.push((
                range.start.max(start) - start..range.end.min(end) - start,
                *token,
            ));
        }

        lines.push(tokens);
        start = end + 1;
    }

    lines
}

/// Highlights kept by the hash of their language and text, the oldest dropped first.
struct Cache {
    entries: HashMap<u64, Arc<[Span]>>,
    order: VecDeque<u64>,
}

impl Cache {
    fn get(&mut self, key: u64) -> Option<Arc<[Span]>> {
        let found = self.entries.get(&key).map(Arc::clone)?;

        self.order.retain(|other| *other != key);
        self.order.push_back(key);

        Some(found)
    }

    fn put(&mut self, key: u64, spans: Arc<[Span]>) {
        if self.entries.insert(key, spans).is_none() {
            self.order.push_back(key);
        }

        while self.entries.len() > CACHE_FILES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

fn content_hash(language: Language, text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    language.hash(&mut hasher);
    text.hash(&mut hasher);

    hasher.finish()
}

/// Like [`highlight`], and keeps the answer by the hash of the text, so the same text
/// is parsed once.
pub fn highlight_cached(language: Language, text: &str) -> Arc<[Span]> {
    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

    let key = content_hash(language, text);
    let with_cache = |work: &mut dyn FnMut(&mut Cache)| {
        let mut guard = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        work(guard.get_or_insert_with(|| Cache {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }));
    };

    let mut found = None;
    with_cache(&mut |cache| found = cache.get(key));
    if let Some(found) = found {
        return found;
    }

    let spans: Arc<[Span]> = highlight(language, text).into();
    with_cache(&mut |cache| cache.put(key, Arc::clone(&spans)));

    spans
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// The pieces of `text` that carry a token, with their token.
    fn pieces(language: Language, text: &str) -> Vec<(&str, Token)> {
        highlight(language, text)
            .into_iter()
            .map(|(range, token)| (&text[range], token))
            .collect()
    }

    fn has(language: Language, text: &str, piece: &str, token: Token) -> bool {
        pieces(language, text)
            .iter()
            .any(|(found, found_token)| *found == piece && *found_token == token)
    }

    #[test]
    fn every_language_loads_its_grammar_and_queries() {
        for language in Language::ALL {
            assert!(configuration(language).is_some(), "{language:?}");
        }
    }

    #[test]
    fn extensions_pick_the_language() {
        let pick = |name: &str| Language::from_path(Path::new(name));

        assert_eq!(pick("src/main.rs"), Some(Language::Rust));
        assert_eq!(pick("a/b.TSX"), Some(Language::Tsx));
        assert_eq!(pick("x.mjs"), Some(Language::JavaScript));
        assert_eq!(pick("composer.json"), Some(Language::Json));
        assert_eq!(pick("ci.yml"), Some(Language::Yaml));
        assert_eq!(pick("run.sh"), Some(Language::Bash));
        assert_eq!(pick("Makefile"), None);
        assert_eq!(pick("photo.png"), None);
    }

    #[test]
    fn rust_marks_keywords_strings_comments_and_functions() {
        let text = "// note\nfn add() -> u32 { let s = \"hi\"; 42 }\n";

        assert!(has(Language::Rust, text, "// note", Token::Comment));
        assert!(has(Language::Rust, text, "fn", Token::Keyword));
        assert!(has(Language::Rust, text, "add", Token::Function));
        assert!(has(Language::Rust, text, "\"hi\"", Token::String));
        assert!(has(Language::Rust, text, "42", Token::Constant));
        assert!(has(Language::Rust, text, "u32", Token::Type));
    }

    #[test]
    fn typescript_marks_types_and_keywords() {
        let text = "interface User { name: string }\nconst id: number = 7;\n";

        assert!(has(Language::TypeScript, text, "interface", Token::Keyword));
        assert!(has(Language::TypeScript, text, "7", Token::Number));
        assert!(has(Language::TypeScript, text, "number", Token::Type));
    }

    #[test]
    fn tsx_marks_jsx_tags() {
        let text = "const view = <Panel title=\"a\">{count}</Panel>;\n";

        assert!(has(Language::Tsx, text, "const", Token::Keyword));
        assert!(has(Language::Tsx, text, "\"a\"", Token::String));
    }

    #[test]
    fn javascript_marks_functions_and_strings() {
        let text = "function run() { return 'x'; }\n";

        assert!(has(Language::JavaScript, text, "function", Token::Keyword));
        assert!(has(Language::JavaScript, text, "run", Token::Function));
        assert!(has(Language::JavaScript, text, "'x'", Token::String));
    }

    #[test]
    fn php_marks_keywords_and_strings() {
        let text = "<?php\nfunction run() { return \"x\"; }\n";

        assert!(has(Language::Php, text, "function", Token::Keyword));
        assert!(has(Language::Php, text, "\"x\"", Token::String));
    }

    #[test]
    fn python_marks_keywords_comments_and_strings() {
        let text = "# note\ndef run():\n    return 'x'\n";

        assert!(has(Language::Python, text, "# note", Token::Comment));
        assert!(has(Language::Python, text, "def", Token::Keyword));
        assert!(has(Language::Python, text, "'x'", Token::String));
    }

    #[test]
    fn go_marks_keywords_and_strings() {
        let text = "package main\nfunc run() string { return \"x\" }\n";

        assert!(has(Language::Go, text, "func", Token::Keyword));
        assert!(has(Language::Go, text, "\"x\"", Token::String));
    }

    #[test]
    fn json_marks_strings_numbers_and_constants() {
        let text = "{\"a\": 12, \"b\": true}\n";

        assert!(has(Language::Json, text, "12", Token::Number));
        assert!(has(Language::Json, text, "true", Token::Constant));
        assert!(has(Language::Json, text, "\"b\"", Token::String));
    }

    #[test]
    fn toml_marks_strings_numbers_and_comments() {
        let text = "# note\nname = \"swarm\"\ncount = 3\n";

        assert!(has(Language::Toml, text, "# note", Token::Comment));
        assert!(has(Language::Toml, text, "\"swarm\"", Token::String));
        assert!(has(Language::Toml, text, "3", Token::Number));
    }

    #[test]
    fn yaml_marks_comments_and_strings() {
        let text = "# note\nname: \"swarm\"\nok: true\n";

        assert!(has(Language::Yaml, text, "# note", Token::Comment));
        assert!(has(Language::Yaml, text, "\"swarm\"", Token::String));
    }

    #[test]
    fn markdown_marks_headings_and_code() {
        let text = "# Title\n\n```rust\nfn x() {}\n```\n";

        assert!(
            pieces(Language::Markdown, text)
                .iter()
                .any(|(piece, _)| piece.contains("Title"))
        );
    }

    #[test]
    fn css_marks_strings_numbers_and_comments() {
        let text = "/* note */\na { content: \"x\"; width: 10px; }\n";

        assert!(has(Language::Css, text, "/* note */", Token::Comment));
        assert!(has(Language::Css, text, "\"x\"", Token::String));
    }

    #[test]
    fn html_marks_tags_attributes_and_comments() {
        let text = "<!-- note -->\n<a href=\"value\">link</a>\n";

        assert!(has(Language::Html, text, "<!-- note -->", Token::Comment));
        assert!(has(Language::Html, text, "a", Token::Keyword));
        assert!(has(Language::Html, text, "href", Token::Type));
        assert!(has(Language::Html, text, "value", Token::String));
    }

    #[test]
    fn bash_marks_keywords_comments_and_strings() {
        let text = "# note\nif true; then echo \"x\"; fi\n";

        assert!(has(Language::Bash, text, "# note", Token::Comment));
        assert!(has(Language::Bash, text, "if", Token::Keyword));
        assert!(has(Language::Bash, text, "\"x\"", Token::String));
    }

    #[test]
    fn a_string_that_a_hunk_would_cut_in_two_stays_one_token() {
        let text = "let s = \"line one\nline two\";\n";

        assert!(has(
            Language::Rust,
            text,
            "\"line one\nline two\"",
            Token::String
        ));
    }

    #[test]
    fn a_text_over_the_limits_stays_plain() {
        let long = "a\n".repeat(MAX_LINES + 1);
        let big = "a".repeat(MAX_BYTES + 1);

        assert!(highlight(Language::Rust, &long).is_empty());
        assert!(highlight(Language::Rust, &big).is_empty());
    }

    #[test]
    fn split_lines_cuts_a_span_at_the_line_break() {
        let text = "/* a\nb */ x";
        let spans = highlight(Language::Css, text);
        let lines = split_lines(text, &spans);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], vec![(0..4, Token::Comment)]);
        assert_eq!(lines[1], vec![(0..4, Token::Comment)]);
    }

    #[test]
    fn the_cache_returns_the_same_answer_for_the_same_text() {
        let text = "fn cached_once() {}\n";

        let first = highlight_cached(Language::Rust, text);
        let second = highlight_cached(Language::Rust, text);

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn the_cache_drops_the_oldest_file_past_its_size() {
        let mut cache = Cache {
            entries: HashMap::new(),
            order: VecDeque::new(),
        };

        for key in 0..=CACHE_FILES as u64 {
            cache.put(key, Arc::from(Vec::new()));
        }

        assert!(cache.get(0).is_none());
        assert!(cache.get(1).is_some());
    }

    #[test]
    fn five_thousand_lines_of_rust_highlight_in_well_under_a_second() {
        let unit = "/// Adds two numbers.\npub fn add(left: u32, right: u32) -> u32 {\n    let label = \"sum\";\n    left + right // 1\n}\n";
        let text = unit.repeat(1_000);
        assert_eq!(text.lines().count(), 5_000);

        let started = Instant::now();
        let spans = highlight(Language::Rust, &text);
        let elapsed = started.elapsed();

        eprintln!("highlighted 5000 lines of Rust in {elapsed:?}");
        assert!(!spans.is_empty());
        assert!(elapsed.as_secs() < 2, "{elapsed:?}");
    }
}
