//! Dependency-free lexical highlighting and folding for languages that do not
//! ship a bundled Tree-sitter grammar.
//!
//! The lexer is deliberately small: comments, strings, numbers, keywords,
//! types, constants, function calls, attributes and a handful of
//! dialect-specific constructs (JSON/TOML/YAML keys, CSS selectors, markup
//! tags, Markdown inline spans). It produces the same `SyntaxToken` shape as
//! the Tree-sitter provider, so presentation code never needs to know which
//! provider produced the tokens.

use crate::{
    clamp_range, clamp_str_range, DocumentSnapshot, FoldRange, FoldingProvider, HighlightResult, LanguageError,
    SyntaxHighlightProvider, SyntaxToken,
};
use std::ops::Range;
use std::sync::Mutex;

/// Documents up to this size are lexed in one pass and cached per version.
const FULL_LEX_BYTES: usize = 2 * 1024 * 1024;
/// Larger documents lex a window of this many lines around the request.
const WINDOW_LINES: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldStyle {
    /// Fold matching `{}`/`[]`/`()` pairs that span several lines.
    Brackets,
    /// Fold blocks of deeper indentation (Python, YAML, markup).
    Indent,
    /// Fold Markdown heading sections.
    Headings,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Code,
    Json,
    Toml,
    Yaml,
    Markup,
    Markdown,
    Css,
    Shell,
}

#[derive(Debug)]
pub struct LanguageSyntax {
    pub id: &'static str,
    pub extensions: &'static [&'static str],
    pub dialect: Dialect,
    pub line_comments: &'static [&'static str],
    pub block_comment: Option<(&'static str, &'static str)>,
    pub quotes: &'static [char],
    pub triple_quotes: bool,
    pub multiline_quotes: &'static [char],
    pub keywords: &'static [&'static str],
    pub types: &'static [&'static str],
    pub constants: &'static [&'static str],
    pub ignore_case: bool,
    pub functions: bool,
    pub capitalized_types: bool,
    pub hash_directives: bool,
    pub fold: FoldStyle,
}

const BASE: LanguageSyntax = LanguageSyntax {
    id: "",
    extensions: &[],
    dialect: Dialect::Code,
    line_comments: &[],
    block_comment: None,
    quotes: &['"', '\''],
    triple_quotes: false,
    multiline_quotes: &[],
    keywords: &[],
    types: &[],
    constants: &["true", "false", "null"],
    ignore_case: false,
    functions: true,
    capitalized_types: true,
    hash_directives: false,
    fold: FoldStyle::Brackets,
};

const C_BLOCK: Option<(&str, &str)> = Some(("/*", "*/"));

const JAVASCRIPT_BASE: LanguageSyntax = LanguageSyntax {
    id: "javascript",
    extensions: &["js", "jsx", "mjs", "cjs"],
    line_comments: &["//"],
    block_comment: C_BLOCK,
    multiline_quotes: &['`'],
    quotes: &['"', '\'', '`'],
    keywords: &[
        "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
        "default", "delete", "do", "else", "export", "extends", "finally", "for", "from",
        "function", "if", "import", "in", "instanceof", "let", "new", "of", "return", "static",
        "super", "switch", "this", "throw", "try", "typeof", "var", "void", "while", "with",
        "yield", "get", "set",
    ],
    constants: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    ..BASE
};

static JAVASCRIPT: LanguageSyntax = JAVASCRIPT_BASE;

static TYPESCRIPT: LanguageSyntax = LanguageSyntax {
    id: "typescript",
    extensions: &["ts", "tsx", "mts", "cts"],
    keywords: &[
        "abstract", "as", "async", "await", "break", "case", "catch", "class", "const",
        "continue", "declare", "default", "delete", "do", "else", "enum", "export", "extends",
        "finally", "for", "from", "function", "if", "implements", "import", "in", "instanceof",
        "interface", "keyof", "let", "namespace", "new", "of", "private", "protected", "public",
        "readonly", "return", "static", "super", "switch", "this", "throw", "try", "type",
        "typeof", "var", "void", "while", "yield", "get", "set", "is", "satisfies",
    ],
    types: &[
        "string", "number", "boolean", "any", "unknown", "never", "object", "symbol", "bigint",
    ],
    ..JAVASCRIPT_BASE
};

static JSON: LanguageSyntax = LanguageSyntax {
    id: "json",
    extensions: &["json", "jsonc", "json5"],
    dialect: Dialect::Json,
    line_comments: &["//"],
    block_comment: C_BLOCK,
    quotes: &['"'],
    functions: false,
    capitalized_types: false,
    ..BASE
};

static TOML: LanguageSyntax = LanguageSyntax {
    id: "toml",
    extensions: &["toml", "ini", "cfg", "conf", "properties"],
    dialect: Dialect::Toml,
    line_comments: &["#", ";"],
    triple_quotes: true,
    functions: false,
    capitalized_types: false,
    constants: &["true", "false", "inf", "nan"],
    fold: FoldStyle::Brackets,
    ..BASE
};

static YAML: LanguageSyntax = LanguageSyntax {
    id: "yaml",
    extensions: &["yaml", "yml"],
    dialect: Dialect::Yaml,
    line_comments: &["#"],
    functions: false,
    capitalized_types: false,
    constants: &["true", "false", "null", "yes", "no", "on", "off"],
    fold: FoldStyle::Indent,
    ..BASE
};

static PYTHON: LanguageSyntax = LanguageSyntax {
    id: "python",
    extensions: &["py", "pyw", "pyi"],
    line_comments: &["#"],
    triple_quotes: true,
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
        "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
        "yield", "match", "case",
    ],
    types: &[
        "int", "float", "str", "bool", "list", "dict", "set", "tuple", "bytes", "object",
    ],
    constants: &["True", "False", "None", "self", "cls"],
    fold: FoldStyle::Indent,
    ..BASE
};

static LUA: LanguageSyntax = LanguageSyntax {
    id: "lua",
    extensions: &["lua"],
    line_comments: &["--"],
    block_comment: Some(("--[[", "]]")),
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in",
        "local", "not", "or", "repeat", "return", "then", "until", "while",
    ],
    constants: &["true", "false", "nil"],
    capitalized_types: false,
    fold: FoldStyle::Brackets,
    ..BASE
};

const C_KEYWORDS: &[&str] = &[
    "auto", "break", "case", "const", "continue", "default", "do", "else", "enum", "extern",
    "for", "goto", "if", "inline", "register", "restrict", "return", "sizeof", "static",
    "struct", "switch", "typedef", "union", "volatile", "while",
];
const C_TYPES: &[&str] = &[
    "void", "char", "short", "int", "long", "float", "double", "signed", "unsigned", "bool",
    "size_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "int8_t", "int16_t", "int32_t",
    "int64_t",
];

static C: LanguageSyntax = C_BASE;

const C_BASE: LanguageSyntax = LanguageSyntax {
    id: "c",
    extensions: &["c", "h"],
    line_comments: &["//"],
    block_comment: C_BLOCK,
    keywords: C_KEYWORDS,
    types: C_TYPES,
    constants: &["true", "false", "NULL", "nullptr"],
    hash_directives: true,
    ..BASE
};

static CPP: LanguageSyntax = LanguageSyntax {
    id: "cpp",
    extensions: &["cc", "cpp", "cxx", "hpp", "hh", "hxx", "inl"],
    keywords: &[
        "alignas", "auto", "break", "case", "catch", "class", "const", "constexpr", "continue",
        "default", "delete", "do", "else", "enum", "explicit", "export", "extern", "final",
        "for", "friend", "goto", "if", "inline", "namespace", "new", "noexcept", "operator",
        "override", "private", "protected", "public", "return", "sizeof", "static", "struct",
        "switch", "template", "this", "throw", "try", "typedef", "typename", "union", "using",
        "virtual", "volatile", "while", "concept", "requires",
    ],
    ..C_BASE
};

static CSHARP: LanguageSyntax = LanguageSyntax {
    id: "csharp",
    extensions: &["cs"],
    line_comments: &["//"],
    block_comment: C_BLOCK,
    keywords: &[
        "abstract", "as", "async", "await", "base", "break", "case", "catch", "class", "const",
        "continue", "default", "do", "else", "enum", "event", "explicit", "extern", "finally",
        "for", "foreach", "get", "if", "implicit", "in", "interface", "internal", "is", "lock",
        "namespace", "new", "operator", "out", "override", "params", "private", "protected",
        "public", "readonly", "ref", "return", "sealed", "set", "static", "struct", "switch",
        "this", "throw", "try", "typeof", "unsafe", "using", "var", "virtual", "void",
        "volatile", "while", "yield", "record",
    ],
    types: &[
        "bool", "byte", "char", "decimal", "double", "float", "int", "long", "object", "sbyte",
        "short", "string", "uint", "ulong", "ushort",
    ],
    constants: &["true", "false", "null"],
    hash_directives: true,
    ..BASE
};

static JAVA: LanguageSyntax = LanguageSyntax {
    id: "java",
    extensions: &["java", "kt", "kts", "scala"],
    line_comments: &["//"],
    block_comment: C_BLOCK,
    keywords: &[
        "abstract", "assert", "break", "case", "catch", "class", "continue", "default", "do",
        "else", "enum", "extends", "final", "finally", "for", "if", "implements", "import",
        "instanceof", "interface", "native", "new", "package", "private", "protected", "public",
        "return", "static", "super", "switch", "synchronized", "this", "throw", "throws",
        "transient", "try", "volatile", "while", "var", "fun", "val", "object",
    ],
    types: &[
        "boolean", "byte", "char", "double", "float", "int", "long", "short", "void", "String",
    ],
    ..BASE
};

static GO: LanguageSyntax = LanguageSyntax {
    id: "go",
    extensions: &["go"],
    line_comments: &["//"],
    block_comment: C_BLOCK,
    multiline_quotes: &['`'],
    quotes: &['"', '\'', '`'],
    keywords: &[
        "break", "case", "chan", "const", "continue", "default", "defer", "else",
        "fallthrough", "for", "func", "go", "goto", "if", "import", "interface", "map",
        "package", "range", "return", "select", "struct", "switch", "type", "var",
    ],
    types: &[
        "bool", "byte", "complex64", "complex128", "error", "float32", "float64", "int",
        "int8", "int16", "int32", "int64", "rune", "string", "uint", "uint8", "uint16",
        "uint32", "uint64", "uintptr", "any",
    ],
    constants: &["true", "false", "nil", "iota"],
    ..BASE
};

static SHADER: LanguageSyntax = LanguageSyntax {
    id: "shader",
    extensions: &[
        "wgsl", "glsl", "vert", "frag", "comp", "geom", "tesc", "tese", "hlsl", "fx", "metal",
        "slang",
    ],
    line_comments: &["//"],
    block_comment: C_BLOCK,
    keywords: &[
        "fn", "let", "var", "const", "struct", "return", "if", "else", "for", "while", "loop",
        "break", "continue", "switch", "case", "default", "discard", "override", "alias",
        "uniform", "varying", "attribute", "in", "out", "inout", "layout", "precision",
        "cbuffer", "register", "technique", "pass", "texture", "sampler", "static", "inline",
        "enable", "requires", "diagnostic",
    ],
    types: &[
        "bool", "i32", "u32", "f32", "f16", "vec2", "vec3", "vec4", "mat2", "mat3", "mat4",
        "mat2x2", "mat3x3", "mat4x4", "array", "ptr", "atomic", "sampler", "texture_2d",
        "texture_3d", "texture_cube", "texture_storage_2d", "float", "float2", "float3",
        "float4", "float3x3", "float4x4", "int", "uint", "int2", "int3", "int4", "uint2",
        "uint3", "uint4", "void", "half", "double", "vec2f", "vec3f", "vec4f", "mat4x4f",
        "vec2i", "vec3i", "vec4i", "vec2u", "vec3u", "vec4u",
    ],
    hash_directives: true,
    ..BASE
};

static SHELL: LanguageSyntax = LanguageSyntax {
    id: "shell",
    extensions: &["sh", "bash", "zsh", "fish", "ps1"],
    dialect: Dialect::Shell,
    line_comments: &["#"],
    keywords: &[
        "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case",
        "esac", "in", "function", "select", "return", "exit", "local", "export", "readonly",
        "declare", "set", "unset", "source", "alias", "echo", "cd", "shift", "trap",
    ],
    functions: false,
    capitalized_types: false,
    ..BASE
};

static RUBY: LanguageSyntax = LanguageSyntax {
    id: "ruby",
    extensions: &["rb", "rake", "gemspec"],
    line_comments: &["#"],
    keywords: &[
        "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do", "else",
        "elsif", "end", "ensure", "for", "if", "in", "module", "next", "not", "or", "redo",
        "rescue", "retry", "return", "self", "super", "then", "undef", "unless", "until",
        "when", "while", "yield", "require", "include", "attr_accessor", "attr_reader",
    ],
    constants: &["true", "false", "nil"],
    fold: FoldStyle::Indent,
    ..BASE
};

static SQL: LanguageSyntax = LanguageSyntax {
    id: "sql",
    extensions: &["sql"],
    line_comments: &["--"],
    block_comment: C_BLOCK,
    ignore_case: true,
    keywords: &[
        "select", "from", "where", "insert", "into", "values", "update", "set", "delete",
        "create", "alter", "drop", "table", "index", "view", "join", "inner", "left", "right",
        "outer", "full", "cross", "on", "group", "by", "order", "having", "limit", "offset",
        "union", "all", "distinct", "as", "and", "or", "not", "in", "is", "like", "between",
        "exists", "case", "when", "then", "else", "end", "primary", "key", "foreign",
        "references", "default", "constraint", "unique", "begin", "commit", "rollback",
        "with", "returning",
    ],
    types: &[
        "int", "integer", "bigint", "smallint", "varchar", "char", "text", "boolean", "date",
        "timestamp", "datetime", "float", "double", "decimal", "numeric", "blob", "json",
    ],
    capitalized_types: false,
    ..BASE
};

static CSS: LanguageSyntax = LanguageSyntax {
    id: "css",
    extensions: &["css", "scss", "less"],
    dialect: Dialect::Css,
    block_comment: C_BLOCK,
    line_comments: &[],
    functions: true,
    capitalized_types: false,
    constants: &["inherit", "initial", "unset", "none", "auto", "important"],
    ..BASE
};

static HTML: LanguageSyntax = HTML_BASE;

const HTML_BASE: LanguageSyntax = LanguageSyntax {
    id: "html",
    extensions: &["html", "htm", "xhtml", "vue", "svelte"],
    dialect: Dialect::Markup,
    fold: FoldStyle::Indent,
    functions: false,
    capitalized_types: false,
    ..BASE
};

static XML: LanguageSyntax = LanguageSyntax {
    id: "xml",
    extensions: &["xml", "svg", "xaml", "xsd", "plist", "csproj", "props", "targets"],
    ..HTML_BASE
};

static MARKDOWN: LanguageSyntax = LanguageSyntax {
    id: "markdown",
    extensions: &["md", "markdown", "mdx"],
    dialect: Dialect::Markdown,
    fold: FoldStyle::Headings,
    functions: false,
    capitalized_types: false,
    ..BASE
};

pub static SYNTAXES: &[&LanguageSyntax] = &[
    &JAVASCRIPT,
    &TYPESCRIPT,
    &JSON,
    &TOML,
    &YAML,
    &PYTHON,
    &LUA,
    &C,
    &CPP,
    &CSHARP,
    &JAVA,
    &GO,
    &SHADER,
    &SHELL,
    &RUBY,
    &SQL,
    &CSS,
    &HTML,
    &XML,
    &MARKDOWN,
];

pub fn syntax_for_extension(extension: &str) -> Option<&'static LanguageSyntax> {
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    SYNTAXES
        .iter()
        .copied()
        .find(|syntax| syntax.extensions.contains(&extension.as_str()))
}

pub fn syntax_for_id(id: &str) -> Option<&'static LanguageSyntax> {
    SYNTAXES.iter().copied().find(|syntax| syntax.id == id)
}

/// Lexed tokens plus fold ranges for one window of text.
#[derive(Debug, Default, Clone)]
pub struct LexOutput {
    pub tokens: Vec<SyntaxToken>,
    pub folds: Vec<FoldRange>,
}

/// Lexes `text[window]`. `base_line` is the zero-based line of `window.start`.
pub fn lex(
    text: &str,
    window: Range<usize>,
    base_line: usize,
    syntax: &LanguageSyntax,
) -> LexOutput {
    let window = clamp_str_range(text, window);
    let mut lexer = Lexer {
        text,
        b: text.as_bytes(),
        syn: syntax,
        pos: window.start,
        end: window.end,
        line: base_line,
        out: Vec::new(),
        folds: Vec::new(),
        stack: Vec::new(),
    };
    match syntax.dialect {
        Dialect::Markdown => lexer.markdown(),
        Dialect::Markup => lexer.markup(),
        _ => lexer.code(),
    }
    let mut folds = lexer.folds;
    match syntax.fold {
        FoldStyle::Indent => {
            folds.extend(indent_folds(&text[window.clone()], base_line, false));
        }
        FoldStyle::Headings => {
            folds.extend(indent_folds(&text[window.clone()], base_line, true));
        }
        _ => {}
    }
    folds.sort_by_key(|fold| (fold.start_line, fold.end_line));
    folds.dedup_by_key(|fold| (fold.start_line, fold.end_line));
    LexOutput {
        tokens: lexer.out,
        folds,
    }
}

struct Lexer<'a> {
    text: &'a str,
    b: &'a [u8],
    syn: &'a LanguageSyntax,
    pos: usize,
    end: usize,
    line: usize,
    out: Vec<SyntaxToken>,
    folds: Vec<FoldRange>,
    stack: Vec<(u8, usize)>,
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$' || byte >= 0x80
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

impl<'a> Lexer<'a> {
    fn push(&mut self, range: Range<usize>, kind: &str) {
        if range.start < range.end {
            self.out.push(SyntaxToken::new(range, kind));
        }
    }

    fn skip_to(&mut self, to: usize) {
        let to = to.min(self.end).max(self.pos);
        self.line += self.b[self.pos..to].iter().filter(|b| **b == b'\n').count();
        self.pos = to;
    }

    fn starts(&self, needle: &str) -> bool {
        self.b[self.pos..self.end].starts_with(needle.as_bytes())
    }

    fn line_end(&self, from: usize) -> usize {
        self.b[from..self.end]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(self.end, |offset| from + offset)
    }

    fn next_nonspace(&self, from: usize) -> Option<u8> {
        self.b[from.min(self.end)..self.end]
            .iter()
            .copied()
            .find(|b| !matches!(b, b' ' | b'\t'))
    }

    fn comment(&mut self) -> bool {
        if let Some((open, close)) = self.syn.block_comment {
            if self.starts(open) {
                let search = self.pos + open.len();
                let stop = find(&self.b[search..self.end], close.as_bytes())
                    .map_or(self.end, |offset| search + offset + close.len());
                self.push(self.pos..stop, "comment");
                self.skip_to(stop);
                return true;
            }
        }
        for prefix in self.syn.line_comments {
            if !self.starts(prefix) {
                continue;
            }
            if *prefix == "#" || *prefix == ";" {
                let previous = self.pos.checked_sub(1).map(|index| self.b[index]);
                if previous.is_some_and(|byte| !byte.is_ascii_whitespace()) {
                    continue;
                }
            }
            let stop = self.line_end(self.pos);
            let doc = *prefix == "//"
                && ((self.starts("///") && !self.starts("////")) || self.starts("//!"));
            self.push(
                self.pos..stop,
                if doc { "comment.documentation" } else { "comment" },
            );
            self.pos = stop;
            return true;
        }
        false
    }

    fn string(&mut self, quote: u8) {
        let start = self.pos;
        let triple = self.syn.triple_quotes && self.b[start..self.end].starts_with(&[quote; 3]);
        let multiline = triple || self.syn.multiline_quotes.contains(&(quote as char));
        let mut index = start + if triple { 3 } else { 1 };
        while index < self.end {
            let byte = self.b[index];
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if triple {
                if self.b[index..self.end].starts_with(&[quote; 3]) {
                    index += 3;
                    break;
                }
            } else if byte == quote {
                index += 1;
                break;
            } else if byte == b'\n' && !multiline {
                break;
            }
            index += 1;
        }
        let mut stop = index.min(self.end);
        while stop < self.end && !self.text.is_char_boundary(stop) {
            stop += 1;
        }
        let key = self.syn.dialect == Dialect::Json && self.next_nonspace(stop) == Some(b':');
        self.push(start..stop, if key { "property" } else { "string" });
        self.skip_to(stop);
    }

    fn number(&mut self) {
        let start = self.pos;
        let hex = self.b[start..self.end].starts_with(b"0x") || self.b[start..self.end].starts_with(b"0X");
        let mut index = start + 1;
        while index < self.end {
            let byte = self.b[index];
            let exponent_sign = !hex
                && matches!(byte, b'+' | b'-')
                && matches!(self.b[index - 1], b'e' | b'E');
            if byte.is_ascii_alphanumeric() || byte == b'_' || exponent_sign {
                index += 1;
            } else if byte == b'.' && self.b.get(index + 1).is_some_and(|next| next.is_ascii_digit()) {
                index += 1;
            } else {
                break;
            }
        }
        self.push(start..index, "number");
        self.pos = index;
    }

    fn word_end(&self, from: usize, dash: bool) -> usize {
        let mut index = from;
        while index < self.end {
            let byte = self.b[index];
            if is_ident_continue(byte) {
                index += 1;
            } else if dash
                && byte == b'-'
                && self.b.get(index + 1).is_some_and(|next| next.is_ascii_alphanumeric())
            {
                index += 1;
            } else {
                break;
            }
        }
        index
    }

    fn contains(&self, list: &[&str], word: &str) -> bool {
        if self.syn.ignore_case {
            list.iter().any(|entry| entry.eq_ignore_ascii_case(word))
        } else {
            list.contains(&word)
        }
    }

    fn classify(&self, word: &str, after: usize) -> Option<&'static str> {
        if self.contains(self.syn.keywords, word) {
            Some("keyword")
        } else if self.contains(self.syn.types, word) {
            Some("type.builtin")
        } else if self.contains(self.syn.constants, word) {
            Some("constant.builtin")
        } else if self.syn.functions && self.next_nonspace(after) == Some(b'(') {
            Some("function")
        } else if self.syn.capitalized_types
            && word.len() > 1
            && word.chars().next().is_some_and(char::is_uppercase)
        {
            Some("type")
        } else {
            None
        }
    }

    /// TOML/YAML `key =` / `key:` at the start of a line.
    fn key_line(&mut self, separator: u8) -> bool {
        let start = self.pos;
        let mut index = start;
        let first = self.b[index];
        if first == b'"' || first == b'\'' {
            index += 1;
            while index < self.end && self.b[index] != first && self.b[index] != b'\n' {
                index += 1;
            }
            if index >= self.end || self.b[index] != first {
                return false;
            }
            index += 1;
        } else {
            while index < self.end
                && !matches!(
                    self.b[index],
                    b' ' | b'\t' | b'\r' | b'\n' | b':' | b'=' | b'#' | b'[' | b']' | b'{'
                        | b'}' | b',' | b'"' | b'\''
                )
            {
                index += 1;
            }
        }
        if index == start {
            return false;
        }
        let key_end = index;
        while index < self.end && matches!(self.b[index], b' ' | b'\t') {
            index += 1;
        }
        if index >= self.end || self.b[index] != separator {
            return false;
        }
        if separator == b':' {
            let after = self.b.get(index + 1).copied();
            if !matches!(after, None | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')) {
                return false;
            }
        }
        self.push(start..key_end, "property");
        self.pos = key_end;
        true
    }

    fn open(&mut self, byte: u8) {
        self.stack.push((byte, self.line));
    }

    fn close(&mut self, byte: u8) {
        let open = match byte {
            b'}' => b'{',
            b']' => b'[',
            _ => b'(',
        };
        let Some(index) = self.stack.iter().rposition(|(candidate, _)| *candidate == open) else {
            return;
        };
        let (_, open_line) = self.stack[index];
        self.stack.truncate(index);
        if self.syn.fold != FoldStyle::Brackets || open_line >= self.line {
            return;
        }
        let line_stop = self.line_end(self.pos);
        let reopens = self.b[(self.pos + 1).min(line_stop)..line_stop]
            .iter()
            .any(|b| matches!(b, b'{' | b'[' | b'('));
        let end = if reopens { self.line } else { self.line + 1 };
        if end > open_line + 1 {
            self.folds.push(FoldRange::new(open_line, end));
        }
    }

    fn code(&mut self) {
        let dialect = self.syn.dialect;
        let css = dialect == Dialect::Css;
        let mut at_start = true;
        while self.pos < self.end {
            let c = self.b[self.pos];
            match c {
                b'\n' => {
                    self.line += 1;
                    self.pos += 1;
                    at_start = true;
                    continue;
                }
                b' ' | b'\t' | b'\r' => {
                    self.pos += 1;
                    continue;
                }
                _ => {}
            }
            let first = std::mem::replace(&mut at_start, false);
            if self.comment() {
                continue;
            }
            match dialect {
                Dialect::Toml if first => {
                    if c == b'[' {
                        let stop = self.line_end(self.pos);
                        let close = self.b[self.pos..stop]
                            .iter()
                            .rposition(|b| *b == b']')
                            .map_or(stop, |offset| self.pos + offset + 1);
                        self.push(self.pos..close, "type");
                        self.pos = close;
                        continue;
                    }
                    if self.key_line(b'=') {
                        continue;
                    }
                }
                Dialect::Yaml => {
                    if c == b'-' && matches!(self.b.get(self.pos + 1), None | Some(b' ') | Some(b'\n')) {
                        self.pos += 1;
                        at_start = true;
                        continue;
                    }
                    if first && self.key_line(b':') {
                        continue;
                    }
                }
                Dialect::Shell if c == b'$' => {
                    let start = self.pos;
                    let mut index = start + 1;
                    match self.b.get(index) {
                        Some(b'{') => {
                            while index < self.end && self.b[index] != b'}' && self.b[index] != b'\n' {
                                index += 1;
                            }
                            index = (index + 1).min(self.end);
                        }
                        Some(b'(') | None => {}
                        Some(byte) if is_ident_start(*byte) => index = self.word_end(index, false),
                        Some(_) => index += 1,
                    }
                    self.push(start..index, "variable");
                    self.pos = index;
                    continue;
                }
                _ => {}
            }
            if self.syn.hash_directives && c == b'#' && first {
                let mut index = self.pos + 1;
                while index < self.end && matches!(self.b[index], b' ' | b'\t') {
                    index += 1;
                }
                let stop = self.word_end(index, false);
                self.push(self.pos..stop.max(self.pos + 1), "keyword");
                self.pos = stop.max(self.pos + 1);
                continue;
            }
            if self.syn.quotes.contains(&(c as char)) {
                self.string(c);
                continue;
            }
            if c.is_ascii_digit()
                || (c == b'.' && self.b.get(self.pos + 1).is_some_and(|b| b.is_ascii_digit()))
            {
                self.number();
                continue;
            }
            if css {
                if c == b'@' {
                    let stop = self.word_end(self.pos + 1, true);
                    self.push(self.pos..stop, "keyword");
                    self.pos = stop.max(self.pos + 1);
                    continue;
                }
                if c == b'#' {
                    let stop = self.word_end(self.pos + 1, true);
                    let hex = self.b[self.pos + 1..stop].iter().all(u8::is_ascii_hexdigit)
                        && matches!(stop - self.pos - 1, 3 | 4 | 6 | 8);
                    self.push(
                        self.pos..stop,
                        if hex && !self.stack.is_empty() { "constant" } else { "type" },
                    );
                    self.pos = stop.max(self.pos + 1);
                    continue;
                }
                if c == b'.' && self.b.get(self.pos + 1).is_some_and(|b| is_ident_start(*b)) {
                    let stop = self.word_end(self.pos + 1, true);
                    self.push(self.pos..stop, "type");
                    self.pos = stop;
                    continue;
                }
                let starts_word = is_ident_start(c)
                    || (c == b'-'
                        && self
                            .b
                            .get(self.pos + 1)
                            .is_some_and(|b| *b == b'-' || b.is_ascii_alphabetic()));
                if starts_word {
                    let stop = self.word_end(self.pos + 1, true);
                    let word = &self.text[self.pos..stop];
                    let kind = if self.next_nonspace(stop) == Some(b':')
                        && self.b.get(stop) != Some(&b':')
                        && !self.stack.is_empty()
                    {
                        Some("property")
                    } else if self.stack.is_empty() {
                        Some("type")
                    } else {
                        self.classify(word, stop)
                    };
                    if let Some(kind) = kind {
                        self.push(self.pos..stop, kind);
                    }
                    self.pos = stop;
                    continue;
                }
            }
            if c == b'@' && self.b.get(self.pos + 1).is_some_and(|b| is_ident_start(*b)) {
                let stop = self.word_end(self.pos + 1, false);
                self.push(self.pos..stop, "attribute");
                self.pos = stop;
                continue;
            }
            if is_ident_start(c) {
                let dash = matches!(dialect, Dialect::Toml | Dialect::Yaml);
                let stop = self.word_end(self.pos, dash);
                let word = &self.text[self.pos..stop];
                if let Some(kind) = self.classify(word, stop) {
                    self.push(self.pos..stop, kind);
                }
                self.pos = stop;
                continue;
            }
            match c {
                b'{' | b'[' | b'(' => self.open(c),
                b'}' | b']' | b')' => self.close(c),
                _ => {}
            }
            self.pos += 1;
        }
    }

    fn markup(&mut self) {
        while self.pos < self.end {
            let c = self.b[self.pos];
            if c == b'\n' {
                self.line += 1;
                self.pos += 1;
                continue;
            }
            if self.starts("<!--") {
                let search = self.pos + 4;
                let stop = find(&self.b[search..self.end], b"-->")
                    .map_or(self.end, |offset| search + offset + 3);
                self.push(self.pos..stop, "comment");
                self.skip_to(stop);
                continue;
            }
            let next = self.b.get(self.pos + 1).copied();
            if c == b'<' && next.is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'/' | b'!' | b'?')) {
                self.pos += 1;
                if matches!(self.b.get(self.pos), Some(b'/' | b'!' | b'?')) {
                    self.pos += 1;
                }
                let name_end = self.name_end(self.pos);
                self.push(self.pos..name_end, "tag");
                self.pos = name_end;
                while self.pos < self.end {
                    let byte = self.b[self.pos];
                    match byte {
                        b'>' => {
                            self.pos += 1;
                            break;
                        }
                        b'\n' => {
                            self.line += 1;
                            self.pos += 1;
                        }
                        b'"' | b'\'' => self.string(byte),
                        b' ' | b'\t' | b'\r' | b'/' | b'=' | b'?' => self.pos += 1,
                        _ => {
                            let stop = self.name_end(self.pos).max(self.pos + 1);
                            self.push(self.pos..stop, "attribute");
                            self.pos = stop;
                        }
                    }
                }
                continue;
            }
            if c == b'&' {
                let stop = self.b[self.pos + 1..self.end.min(self.pos + 12)]
                    .iter()
                    .position(|b| *b == b';')
                    .map(|offset| self.pos + offset + 2);
                if let Some(stop) = stop {
                    self.push(self.pos..stop, "constant");
                    self.pos = stop;
                    continue;
                }
            }
            self.pos += 1;
        }
    }

    fn name_end(&self, from: usize) -> usize {
        let mut index = from;
        while index < self.end
            && (self.b[index].is_ascii_alphanumeric()
                || matches!(self.b[index], b':' | b'_' | b'.' | b'-' | b'@')
                || self.b[index] >= 0x80)
        {
            index += 1;
        }
        index
    }

    fn markdown(&mut self) {
        let mut fenced = false;
        while self.pos < self.end {
            let start = self.pos;
            let stop = self.line_end(start);
            let line = &self.text[start..stop];
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                fenced = !fenced;
                self.push(start..stop, "string");
            } else if fenced {
                self.push(start..stop, "string");
            } else if let Some(level) = heading_level(trimmed) {
                let _ = level;
                self.push(start + indent..stop, "title");
            } else {
                let mut inline_start = start + indent;
                if let Some(marker) = list_marker(trimmed) {
                    self.push(inline_start..inline_start + marker, "punctuation.list_marker");
                    inline_start += marker;
                } else if trimmed.starts_with('>') {
                    self.push(inline_start..inline_start + 1, "punctuation.list_marker");
                    inline_start += 1;
                }
                self.markdown_inline(inline_start, stop);
            }
            self.pos = stop;
            if self.pos < self.end {
                self.pos += 1;
                self.line += 1;
            }
        }
    }

    fn markdown_inline(&mut self, from: usize, to: usize) {
        let mut index = from;
        while index < to {
            let rest = &self.b[index..to];
            if rest[0] == b'`' {
                if let Some(close) = rest[1..].iter().position(|b| *b == b'`') {
                    self.push(index..index + close + 2, "string");
                    index += close + 2;
                    continue;
                }
            } else if rest.starts_with(b"**") {
                if let Some(close) = find(&rest[2..], b"**") {
                    self.push(index..index + close + 4, "emphasis.strong");
                    index += close + 4;
                    continue;
                }
            } else if (rest[0] == b'*' || rest[0] == b'_')
                && rest.get(1).is_some_and(|b| !b.is_ascii_whitespace() && *b != rest[0])
            {
                if let Some(close) = rest[1..].iter().position(|b| *b == rest[0]) {
                    if close > 0 {
                        self.push(index..index + close + 2, "emphasis");
                        index += close + 2;
                        continue;
                    }
                }
            } else if rest[0] == b'[' {
                if let Some(close) = rest.iter().position(|b| *b == b']') {
                    if rest.get(close + 1) == Some(&b'(') {
                        if let Some(url_close) = rest[close + 1..].iter().position(|b| *b == b')') {
                            self.push(index..index + close + 1, "link_text");
                            self.push(
                                index + close + 1..index + close + 1 + url_close + 1,
                                "link_uri",
                            );
                            index += close + url_close + 2;
                            continue;
                        }
                    }
                }
            }
            index += 1;
            while index < to && !self.text.is_char_boundary(index) {
                index += 1;
            }
        }
    }
}

fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    (1..=6).contains(&hashes).then_some(hashes).filter(|_| {
        line.as_bytes()
            .get(hashes)
            .map_or(true, |b| *b == b' ' || *b == b'\t')
    })
}

fn list_marker(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    if matches!(bytes.first(), Some(b'-' | b'*' | b'+')) && bytes.get(1) == Some(&b' ') {
        return Some(1);
    }
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    (digits > 0 && matches!(bytes.get(digits), Some(b'.' | b')')) && bytes.get(digits + 1) == Some(&b' '))
        .then_some(digits + 1)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Indentation-based fold regions. When `headings` is set, Markdown headings
/// open sections that end at the next heading of equal or higher rank.
fn indent_folds(text: &str, base_line: usize, headings: bool) -> Vec<FoldRange> {
    let mut folds = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut last_content: Option<usize> = None;
    let mut fenced = false;
    let close = |stack: &mut Vec<(usize, usize)>, last: Option<usize>, threshold: usize, folds: &mut Vec<FoldRange>| {
        while stack.last().is_some_and(|(_, indent)| *indent >= threshold) {
            let (line, _) = stack.pop().unwrap();
            if let Some(last) = last {
                if last > line {
                    folds.push(FoldRange::new(base_line + line, base_line + last + 1));
                }
            }
        }
    };
    for (line_index, line) in text.split('\n').enumerate() {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.trim_end().is_empty() {
            continue;
        }
        let indent = if headings {
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                fenced = !fenced;
            }
            match heading_level(trimmed) {
                Some(level) if !fenced => level,
                _ => 7,
            }
        } else {
            line[..line.len() - trimmed.len()]
                .chars()
                .map(|c| if c == '\t' { 4 } else { 1 })
                .sum()
        };
        close(&mut stack, last_content, indent, &mut folds);
        stack.push((line_index, indent));
        last_content = Some(line_index);
    }
    close(&mut stack, last_content, 0, &mut folds);
    folds
}

struct Cache {
    version: u64,
    window: Range<usize>,
    tokens: Vec<SyntaxToken>,
    folds: Vec<FoldRange>,
}

/// Lexer-backed provider for every non-Rust language. Results are cached per
/// document version, so scrolling never re-lexes.
pub struct GenericSyntaxProvider {
    syntax: &'static LanguageSyntax,
    cache: Mutex<Option<Cache>>,
}

impl std::fmt::Debug for GenericSyntaxProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GenericSyntaxProvider")
            .field("language", &self.syntax.id)
            .finish_non_exhaustive()
    }
}

impl GenericSyntaxProvider {
    pub fn new(syntax: &'static LanguageSyntax) -> Self {
        Self {
            syntax,
            cache: Mutex::new(None),
        }
    }

    pub fn syntax(&self) -> &'static LanguageSyntax {
        self.syntax
    }

    fn with_cache<R>(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
        f: impl FnOnce(&Cache) -> R,
    ) -> Result<R, LanguageError> {
        let text = snapshot.text();
        let need = clamp_range(text, range);
        let mut guard = self
            .cache
            .lock()
            .map_err(|_| LanguageError::Provider("lexer cache lock poisoned".into()))?;
        let valid = guard.as_ref().is_some_and(|cache| {
            cache.version == snapshot.version()
                && cache.window.start <= need.start
                && cache.window.end >= need.end
        });
        if !valid {
            let map = snapshot.position_map();
            let window = if text.len() <= FULL_LEX_BYTES {
                0..text.len()
            } else {
                let first = map.byte_to_line(need.start).unwrap_or(0).saturating_sub(WINDOW_LINES);
                let last = map
                    .byte_to_line(need.end)
                    .unwrap_or(first)
                    .saturating_add(WINDOW_LINES)
                    .min(map.line_count().saturating_sub(1));
                let start = map.line_start(first).unwrap_or(0);
                let end = map.line_end(last).unwrap_or(text.len());
                start..end
            };
            let base_line = map.byte_to_line(window.start).unwrap_or(0);
            // Only the window is read out of the rope; tokens are shifted
            // back to document offsets.
            let source = text.slice(window.clone());
            let mut output = lex(&source, 0..source.len(), base_line, self.syntax);
            for token in &mut output.tokens {
                token.range = token.range.start + window.start..token.range.end + window.start;
            }
            *guard = Some(Cache {
                version: snapshot.version(),
                window,
                tokens: output.tokens,
                folds: output.folds,
            });
        }
        Ok(f(guard.as_ref().expect("cache populated")))
    }
}

impl SyntaxHighlightProvider for GenericSyntaxProvider {
    fn highlight(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> Result<HighlightResult, LanguageError> {
        let range = clamp_range(snapshot.text(), range);
        let version = snapshot.version();
        self.with_cache(snapshot, range.clone(), |cache| {
            let first = cache.tokens.partition_point(|token| token.range.end <= range.start);
            let last = cache.tokens.partition_point(|token| token.range.start < range.end);
            let tokens = cache.tokens[first.min(last)..last]
                .iter()
                .map(|token| {
                    SyntaxToken::new(
                        token.range.start.max(range.start)..token.range.end.min(range.end),
                        token.kind.as_str(),
                    )
                })
                .collect();
            HighlightResult {
                version,
                range: range.clone(),
                tokens,
            }
        })
    }
}

impl FoldingProvider for GenericSyntaxProvider {
    fn fold_ranges(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> Result<Vec<FoldRange>, LanguageError> {
        let map = snapshot.position_map();
        let range = clamp_range(snapshot.text(), range);
        let first = map.byte_to_line(range.start).unwrap_or(0);
        let last = map.byte_to_line(range.end).unwrap_or(first);
        self.with_cache(snapshot, range, |cache| {
            cache
                .folds
                .iter()
                .filter(|fold| fold.start_line <= last && fold.end_line > first)
                .cloned()
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str, syntax: &LanguageSyntax) -> Vec<(String, String)> {
        lex(source, 0..source.len(), 0, syntax)
            .tokens
            .into_iter()
            .map(|token| {
                (
                    source[token.range].to_owned(),
                    token.kind.as_str().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn javascript_tokens_cover_core_categories() {
        let tokens = kinds("const x = 42; // note\nfoo(\"hi\");", &JAVASCRIPT);
        assert!(tokens.contains(&("const".into(), "keyword".into())));
        assert!(tokens.contains(&("42".into(), "number".into())));
        assert!(tokens.contains(&("// note".into(), "comment".into())));
        assert!(tokens.contains(&("foo".into(), "function".into())));
        assert!(tokens.contains(&("\"hi\"".into(), "string".into())));
    }

    #[test]
    fn json_keys_are_properties() {
        let tokens = kinds("{\"a\": \"b\", \"n\": 1}", &JSON);
        assert!(tokens.contains(&("\"a\"".into(), "property".into())));
        assert!(tokens.contains(&("\"b\"".into(), "string".into())));
    }

    #[test]
    fn toml_sections_and_keys() {
        let tokens = kinds("[package]\nname = \"x\"\n", &TOML);
        assert!(tokens.contains(&("[package]".into(), "type".into())));
        assert!(tokens.contains(&("name".into(), "property".into())));
    }

    #[test]
    fn brackets_fold_across_lines_and_keep_else_visible() {
        let output = lex("fn a() {\n  x;\n}\nif b {\n  y\n} else {\n  z\n}\n", 0..42, 0, &C);
        assert!(output.folds.iter().any(|fold| fold.start_line == 0 && fold.end_line == 3));
        assert!(output.folds.iter().any(|fold| fold.start_line == 3 && fold.end_line == 5));
    }

    #[test]
    fn python_indent_folds() {
        let source = "def f():\n    a = 1\n    b = 2\n\nx = 3\n";
        let output = lex(source, 0..source.len(), 0, &PYTHON);
        assert!(output.folds.iter().any(|fold| fold.start_line == 0 && fold.end_line == 3));
    }

    #[test]
    fn markdown_headings_and_sections() {
        let source = "# Title\ntext **bold**\n## Sub\nmore\n";
        let output = lex(source, 0..source.len(), 0, &MARKDOWN);
        assert!(output.tokens.iter().any(|token| token.kind.as_str() == "title"));
        assert!(output.folds.iter().any(|fold| fold.start_line == 0));
    }

    #[test]
    fn extension_lookup() {
        assert_eq!(syntax_for_extension("TS").unwrap().id, "typescript");
        assert_eq!(syntax_for_extension("wgsl").unwrap().id, "shader");
        assert!(syntax_for_extension("rs").is_none());
    }
}
