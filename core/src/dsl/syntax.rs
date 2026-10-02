//! Tokens and syntax tree of the schema language.
//!
//! The grammar is small and newline-insensitive:
//!
//! ```text
//! file      := item*
//! item      := "model" IDENT attr* "{" member* "}"
//!            | "extension" (IDENT | STRING) args?
//!            | "function" IDENT "{" (IDENT ":" value)* "}"
//!            | "import" STRING
//! member    := IDENT ":" type attr*          (a field or relation)
//!            | "@@" IDENT args?               (a model-level declaration)
//! type      := IDENT args? "[]"? "?"?
//! attr      := "@" IDENT args?
//! args      := "(" (arg ("," arg)*)? ","? ")"
//! arg       := IDENT ":" value | value
//! value     := STRING | RAW_STRING | NUMBER | "true" | "false"
//!            | IDENT ("." IDENT)* args?       (names, paths, calls: now, Post.author_id, sql("..."))
//!            | "[" (value ("," value)*)? ","? "]"
//!            | "{" (key ":" value ("," key ":" value)*)? ","? "}"
//! ```
//!
//! Strings are `"..."` with `\"`, `\\`, `\n`, `\t` escapes, or `"""..."""` taken
//! verbatim (for SQL bodies). Comments are `// ...` and `/* ... */`.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

#[derive(Debug)]
pub struct Error {
    pub pos: Pos,
    pub msg: String,
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn err<T>(pos: Pos, msg: impl Into<String>) -> Result<T> {
    Err(Error { pos, msg: msg.into() })
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Punct(&'static str),
    Eof,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    rest: &'a str,
    pos: Pos,
}

impl<'a> Lexer<'a> {
    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        self.rest = &self.rest[c.len_utf8()..];
        if c == '\n' {
            self.pos.line += 1;
            self.pos.col = 1;
        } else {
            self.pos.col += 1;
        }
        Some(c)
    }

    fn skip_trivia(&mut self) -> Result<()> {
        loop {
            match self.chars.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.rest.starts_with("//") => {
                    while self.chars.peek().is_some_and(|c| *c != '\n') {
                        self.bump();
                    }
                }
                Some('/') if self.rest.starts_with("/*") => {
                    let start = self.pos;
                    self.bump();
                    self.bump();
                    loop {
                        if self.rest.starts_with("*/") {
                            self.bump();
                            self.bump();
                            break;
                        }
                        if self.bump().is_none() {
                            return err(start, "unterminated comment");
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn next(&mut self) -> Result<(Pos, Tok)> {
        self.skip_trivia()?;
        let pos = self.pos;
        let Some(&c) = self.chars.peek() else { return Ok((pos, Tok::Eof)) };
        if c.is_alphabetic() || c == '_' {
            let mut s = String::new();
            while let Some(&c) = self.chars.peek() {
                if c.is_alphanumeric() || c == '_' {
                    s.push(c);
                    self.bump();
                } else {
                    break;
                }
            }
            return Ok((pos, Tok::Ident(s)));
        }
        if c.is_ascii_digit() || (c == '-' && self.rest[1..].starts_with(|d: char| d.is_ascii_digit())) {
            let mut s = String::new();
            s.push(c);
            self.bump();
            while let Some(&c) = self.chars.peek() {
                if c.is_ascii_digit() || c == '.' || c == '_' || c == 'e' || c == 'E' {
                    if c != '_' {
                        s.push(c);
                    }
                    self.bump();
                } else {
                    break;
                }
            }
            if s.parse::<f64>().is_err() {
                return err(pos, format!("invalid number {s}"));
            }
            return Ok((pos, Tok::Num(s)));
        }
        if c == '"' {
            if self.rest.starts_with("\"\"\"") {
                for _ in 0..3 {
                    self.bump();
                }
                let Some(end) = self.rest.find("\"\"\"") else {
                    return err(pos, "unterminated \"\"\" string");
                };
                let body = self.rest[..end].to_owned();
                for _ in 0..body.chars().count() + 3 {
                    self.bump();
                }
                return Ok((pos, Tok::Str(dedent(&body))));
            }
            self.bump();
            let mut s = String::new();
            loop {
                match self.bump() {
                    None | Some('\n') => return err(pos, "unterminated string"),
                    Some('"') => break,
                    Some('\\') => match self.bump() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some(c @ ('"' | '\\')) => s.push(c),
                        other => return err(self.pos, format!("unknown escape \\{}", other.unwrap_or(' '))),
                    },
                    Some(c) => s.push(c),
                }
            }
            return Ok((pos, Tok::Str(s)));
        }
        for p in ["@@", "[]", "@", "{", "}", "(", ")", "[", "]", ",", ":", "?", "."] {
            if self.rest.starts_with(p) {
                for _ in 0..p.len() {
                    self.bump();
                }
                return Ok((pos, Tok::Punct(p)));
            }
        }
        err(pos, format!("unexpected character {c:?}"))
    }
}

/// Strips the common indentation of a `"""` block and its first / last blank lines.
fn dedent(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out: Vec<&str> = lines.iter().map(|l| if l.len() >= indent { &l[indent..] } else { l.trim() }).collect();
    while out.first().is_some_and(|l| l.trim().is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------------------
// Syntax tree
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Num(String),
    Bool(bool),
    /// `name`, `Post.author_id`, `sql("...")`, `created_at(sort: desc)`
    Path(Vec<String>, Option<Args>),
    List(Vec<(Pos, Value)>),
    Object(Vec<(String, Pos, Value)>),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Args {
    pub pos: Pos,
    pub positional: Vec<(Pos, Value)>,
    pub named: Vec<(String, Pos, Value)>,
}

#[derive(Debug)]
pub struct Attr {
    pub pos: Pos,
    pub name: String,
    pub args: Args,
}

#[derive(Debug)]
pub struct TypeRef {
    pub pos: Pos,
    pub name: String,
    pub args: Args,
    pub list: bool,
    pub optional: bool,
}

#[derive(Debug)]
pub struct Member {
    pub pos: Pos,
    pub name: String,
    pub ty: TypeRef,
    pub attrs: Vec<Attr>,
}

#[derive(Debug)]
pub struct ModelDecl {
    pub pos: Pos,
    pub name: String,
    pub attrs: Vec<Attr>,
    pub members: Vec<Member>,
    /// `@@...` declarations
    pub blocks: Vec<Attr>,
}

#[derive(Debug)]
pub enum Item {
    Model(ModelDecl),
    Extension { pos: Pos, name: String, args: Args },
    Function { pos: Pos, name: String, props: Vec<(String, Pos, Value)> },
    Import { pos: Pos, path: String },
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    tok: Tok,
    pos: Pos,
}

impl Parser<'_> {
    fn advance(&mut self) -> Result<(Pos, Tok)> {
        let (pos, tok) = self.lexer.next()?;
        let prev = std::mem::replace(&mut self.tok, tok);
        let prev_pos = std::mem::replace(&mut self.pos, pos);
        Ok((prev_pos, prev))
    }

    fn is(&self, p: &str) -> bool {
        matches!(&self.tok, Tok::Punct(q) if *q == p)
    }

    fn eat(&mut self, p: &str) -> Result<bool> {
        if self.is(p) {
            self.advance()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn expect(&mut self, p: &str) -> Result<Pos> {
        if self.is(p) {
            return Ok(self.advance()?.0);
        }
        err(self.pos, format!("expected `{p}`, found {}", describe(&self.tok)))
    }

    fn ident(&mut self, what: &str) -> Result<(Pos, String)> {
        match &self.tok {
            Tok::Ident(_) => match self.advance()? {
                (pos, Tok::Ident(s)) => Ok((pos, s)),
                _ => unreachable!(),
            },
            other => err(self.pos, format!("expected {what}, found {}", describe(other))),
        }
    }

    fn file(&mut self) -> Result<Vec<Item>> {
        let mut items = vec![];
        loop {
            let pos = self.pos;
            match &self.tok {
                Tok::Eof => return Ok(items),
                Tok::Ident(k) if k == "model" => {
                    self.advance()?;
                    items.push(Item::Model(self.model(pos)?));
                }
                Tok::Ident(k) if k == "extension" => {
                    self.advance()?;
                    let name = match self.advance()? {
                        (_, Tok::Ident(s) | Tok::Str(s)) => s,
                        (p, t) => return err(p, format!("expected an extension name, found {}", describe(&t))),
                    };
                    let args = if self.is("(") { self.args()? } else { Args::default() };
                    items.push(Item::Extension { pos, name, args });
                }
                Tok::Ident(k) if k == "function" => {
                    self.advance()?;
                    let (_, name) = self.ident("a function name")?;
                    let props = self.object()?;
                    items.push(Item::Function { pos, name, props });
                }
                Tok::Ident(k) if k == "import" => {
                    self.advance()?;
                    match self.advance()? {
                        (_, Tok::Str(path)) => items.push(Item::Import { pos, path }),
                        (p, t) => return err(p, format!("expected a file path string, found {}", describe(&t))),
                    }
                }
                other => {
                    return err(pos, format!("expected `model`, `extension`, `function` or `import`, found {}", describe(other)))
                }
            }
        }
    }

    fn attrs(&mut self) -> Result<Vec<Attr>> {
        let mut out = vec![];
        while self.is("@") {
            let pos = self.advance()?.0;
            let (_, name) = self.ident("an attribute name")?;
            let args = if self.is("(") { self.args()? } else { Args { pos, ..Default::default() } };
            out.push(Attr { pos, name, args });
        }
        Ok(out)
    }

    fn model(&mut self, pos: Pos) -> Result<ModelDecl> {
        let (_, name) = self.ident("a model name")?;
        let attrs = self.attrs()?;
        self.expect("{")?;
        let (mut members, mut blocks) = (vec![], vec![]);
        loop {
            if self.eat("}")? {
                break;
            }
            if self.is("@@") {
                let pos = self.advance()?.0;
                let (_, bname) = self.ident("a declaration name (index, unique, check, exclude, trigger)")?;
                let args = if self.is("(") { self.args()? } else { Args { pos, ..Default::default() } };
                blocks.push(Attr { pos, name: bname, args });
                continue;
            }
            if self.tok == Tok::Eof {
                return err(pos, format!("model {name} is not closed with `}}`"));
            }
            let (mpos, mname) = self.ident("a field name, `@@` or `}`")?;
            self.expect(":")?;
            let (tpos, tname) = self.ident("a type")?;
            let targs = if self.is("(") { self.args()? } else { Args { pos: tpos, ..Default::default() } };
            let list = self.eat("[]")?;
            let optional = self.eat("?")?;
            let ty = TypeRef { pos: tpos, name: tname, args: targs, list, optional };
            let attrs = self.attrs()?;
            members.push(Member { pos: mpos, name: mname, ty, attrs });
        }
        Ok(ModelDecl { pos, name, attrs, members, blocks })
    }

    fn args(&mut self) -> Result<Args> {
        let pos = self.expect("(")?;
        let mut args = Args { pos, ..Default::default() };
        while !self.is(")") {
            let vpos = self.pos;
            // `name: value` or a value
            let named = matches!(&self.tok, Tok::Ident(_)) && self.peek_is_colon()?;
            if named {
                let (_, key) = self.ident("an argument name")?;
                self.expect(":")?;
                let v = self.value()?;
                if args.named.iter().any(|(k, _, _)| *k == key) {
                    return err(vpos, format!("argument {key} given twice"));
                }
                args.named.push((key, vpos, v));
            } else {
                if !args.named.is_empty() {
                    return err(vpos, "positional argument after named arguments");
                }
                let v = self.value()?;
                args.positional.push((vpos, v));
            }
            if !self.eat(",")? {
                break;
            }
        }
        self.expect(")")?;
        Ok(args)
    }

    /// True if the token after the current identifier is `:`.
    fn peek_is_colon(&mut self) -> Result<bool> {
        let mut probe = Lexer { chars: self.lexer.rest.chars().peekable(), rest: self.lexer.rest, pos: self.lexer.pos };
        Ok(matches!(probe.next()?.1, Tok::Punct(":")))
    }

    fn object(&mut self) -> Result<Vec<(String, Pos, Value)>> {
        self.expect("{")?;
        let mut out: Vec<(String, Pos, Value)> = vec![];
        while !self.is("}") {
            let pos = self.pos;
            let key = match self.advance()? {
                (_, Tok::Ident(s) | Tok::Str(s)) => s,
                (p, t) => return err(p, format!("expected a key, found {}", describe(&t))),
            };
            self.expect(":")?;
            let v = self.value()?;
            if out.iter().any(|(k, _, _)| *k == key) {
                return err(pos, format!("key {key} given twice"));
            }
            out.push((key, pos, v));
            // commas between entries are optional (one entry per line reads well)
            self.eat(",")?;
        }
        self.expect("}")?;
        Ok(out)
    }

    fn value(&mut self) -> Result<Value> {
        let pos = self.pos;
        if self.is("[") {
            self.advance()?;
            let mut items = vec![];
            while !self.is("]") {
                let p = self.pos;
                items.push((p, self.value()?));
                if !self.eat(",")? {
                    break;
                }
            }
            self.expect("]")?;
            return Ok(Value::List(items));
        }
        if self.is("[]") {
            self.advance()?;
            return Ok(Value::List(vec![]));
        }
        if self.is("{") {
            return Ok(Value::Object(self.object()?));
        }
        match self.advance()? {
            (_, Tok::Str(s)) => Ok(Value::Str(s)),
            (_, Tok::Num(n)) => Ok(Value::Num(n)),
            (_, Tok::Ident(s)) if s == "true" => Ok(Value::Bool(true)),
            (_, Tok::Ident(s)) if s == "false" => Ok(Value::Bool(false)),
            (_, Tok::Ident(s)) => {
                let mut path = vec![s];
                while self.eat(".")? {
                    path.push(self.ident("a name after `.`")?.1);
                }
                let args = if self.is("(") { Some(self.args()?) } else { None };
                Ok(Value::Path(path, args))
            }
            (_, t) => err(pos, format!("expected a value, found {}", describe(&t))),
        }
    }
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Ident(s) => format!("`{s}`"),
        Tok::Str(_) => "a string".into(),
        Tok::Num(n) => format!("number {n}"),
        Tok::Punct(p) => format!("`{p}`"),
        Tok::Eof => "end of file".into(),
    }
}

pub fn parse(source: &str) -> Result<Vec<Item>> {
    let mut lexer = Lexer { chars: source.chars().peekable(), rest: source, pos: Pos { line: 1, col: 1 } };
    let (pos, tok) = lexer.next()?;
    let mut p = Parser { lexer, tok, pos };
    p.file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_models_and_values() {
        let items = parse(
            r#"
            // a comment
            model User @table("users") {
                id: BigInt @primary @auto
                email: String(254)? @unique /* inline */
                posts: Post[] @relation(via: Post.author_id)
                @@index([email(sort: desc), sql("lower(email)")], where: "id > 0", with: {m: 16})
            }
            function f { returns: trigger, body: """
                BEGIN
                  RETURN NEW;
                END;
            """ }
            extension "uuid-ossp"(schema: "ext")
            import "x.toml"
            "#,
        )
        .unwrap();
        assert_eq!(items.len(), 4);
        let Item::Model(m) = &items[0] else { panic!() };
        assert_eq!(m.members[1].ty.name, "String");
        assert!(m.members[1].ty.optional && !m.members[1].ty.list);
        assert!(m.members[2].ty.list);
        assert_eq!(m.blocks[0].name, "index");
        assert_eq!(m.blocks[0].args.named.len(), 2);
        let Item::Function { props, .. } = &items[1] else { panic!() };
        assert_eq!(props[1].2, Value::Str("BEGIN\n  RETURN NEW;\nEND;".into()));
    }

    #[test]
    fn reports_positions() {
        let e = parse("model User {\n  id BigInt\n}").unwrap_err();
        assert_eq!((e.pos.line, e.pos.col), (2, 6));
        assert!(e.msg.contains("expected `:`"), "{}", e.msg);
        let e = parse("model X { a: \"oops }").unwrap_err();
        assert!(e.msg.contains("unterminated"));
    }
}
