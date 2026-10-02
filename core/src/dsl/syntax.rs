//! Tokens and syntax tree of the schema language (`.prisma` files).
//!
//! The grammar is Prisma's, with our attributes and blocks added (see
//! `docs/prisma-syntax.md`):
//!
//! ```text
//! file      := item*
//! item      := "model" IDENT "{" member* "}"
//!            | "enum" IDENT "{" (IDENT attr* | "@@" NAME args?)* "}"
//!            | "datasource" IDENT "{" prop* "}"
//!            | "generator" IDENT "{" prop* "}"      (read and ignored)
//!            | "function" IDENT "{" prop* "}"
//!            | "import" STRING
//! prop      := IDENT "=" value
//! member    := IDENT type attr*                  (a field or relation, on one line)
//!            | "@@" NAME args?                    (a model attribute, on one line)
//! type      := IDENT args? "[]"? "?"?
//! attr      := "@" NAME args?
//! NAME      := IDENT ("." IDENT)*                 (`map`, `db.VarChar`)
//! args      := "(" (arg ("," arg)*)? ","? ")"
//! arg       := IDENT ":" value | value
//! value     := STRING | NUMBER | "true" | "false"
//!            | IDENT ("." IDENT)* args?           (names and calls: Cascade, now(), raw("..."))
//!            | "[" (value ("," value)*)? ","? "]"
//!            | "{" (key ":" value ("," key ":" value)*)? ","? "}"
//! ```
//!
//! Strings are `"..."` with `\"`, `\\`, `\n`, `\t` escapes, or `"""..."""` taken
//! verbatim (for SQL bodies). Comments are `// ...`.
//!
//! Two rules keep a file stable under the Prisma formatter (format-on-save):
//! each field and each attribute is on one line, and `"""` only appears in the
//! `key = value` blocks (`function`, `datasource`), never inside a model.

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
    /// `"""..."""`
    Text(String),
    Num(String),
    Punct(&'static str),
    Eof,
}

#[derive(Clone)]
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

    fn skip_trivia(&mut self) {
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
                _ => return,
            }
        }
    }

    fn next(&mut self) -> Result<(Pos, Tok)> {
        self.skip_trivia();
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
                return Ok((pos, Tok::Text(dedent(&body))));
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
        for p in ["@@", "[]", "@", "{", "}", "(", ")", "[", "]", ",", ":", "?", ".", "="] {
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
    /// `name`, `Cascade`, `now()`, `raw("...")`, `created_at(sort: Desc)`
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
    /// `id`, `default`, `db.VarChar`, ...
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
    pub members: Vec<Member>,
    /// `@@...` attributes
    pub blocks: Vec<Attr>,
}

/// `enum Name { value @map("x") ... @@map("db_name") }`
#[derive(Debug)]
pub struct EnumDecl {
    pub pos: Pos,
    pub name: String,
    /// (position, name, attributes) per value
    pub values: Vec<(Pos, String, Vec<Attr>)>,
    /// `@@...` attributes
    pub blocks: Vec<Attr>,
}

pub type Props = Vec<(String, Pos, Value)>;

#[derive(Debug)]
pub enum Item {
    Model(ModelDecl),
    Enum(EnumDecl),
    Datasource { pos: Pos, props: Props },
    Function { pos: Pos, name: String, props: Props },
    Import { pos: Pos, path: String },
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    tok: Tok,
    pos: Pos,
    /// Where the current token ends.
    end: Pos,
    /// Where the last consumed token ended.
    prev_end: Pos,
    /// Inside a model: `"""` strings are refused.
    in_model: bool,
}

impl Parser<'_> {
    fn advance(&mut self) -> Result<(Pos, Tok)> {
        let (pos, tok) = self.lexer.next()?;
        self.prev_end = std::mem::replace(&mut self.end, self.lexer.pos);
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
            let keyword = match &self.tok {
                Tok::Eof => return Ok(items),
                Tok::Ident(k) => k.clone(),
                other => return err(pos, format!("expected a block (`model`, `datasource`, `function`, ...), found {}", describe(other))),
            };
            self.advance()?;
            match keyword.as_str() {
                "model" => {
                    let (_, name) = self.ident("a model name")?;
                    self.in_model = true;
                    let m = self.model(pos, name)?;
                    self.in_model = false;
                    items.push(Item::Model(m));
                }
                "datasource" => {
                    self.ident("a datasource name")?;
                    let props = self.props("datasource")?;
                    items.push(Item::Datasource { pos, props });
                }
                // Prisma's own generators (`prisma-client-js`, ...) may share the file.
                "generator" => {
                    self.ident("a generator name")?;
                    self.props("generator")?;
                }
                "function" => {
                    let (_, name) = self.ident("a function name")?;
                    let props = self.props(&format!("function {name}"))?;
                    items.push(Item::Function { pos, name, props });
                }
                "import" => match self.advance()? {
                    (_, Tok::Str(path)) => items.push(Item::Import { pos, path }),
                    (p, t) => return err(p, format!("expected a file path string, found {}", describe(&t))),
                },
                "enum" => {
                    let (_, name) = self.ident("an enum name")?;
                    self.in_model = true;
                    let e = self.enum_block(pos, name)?;
                    self.in_model = false;
                    items.push(Item::Enum(e));
                }
                "type" | "view" => return err(pos, format!("`{keyword}` blocks aren't supported yet")),
                _ => {
                    return err(
                        pos,
                        format!("expected `model`, `enum`, `datasource`, `generator`, `function` or `import`, found `{keyword}`"),
                    )
                }
            }
        }
    }

    /// `{ key = value ... }`
    fn props(&mut self, what: &str) -> Result<Props> {
        self.expect("{")?;
        let mut out: Props = vec![];
        while !self.eat("}")? {
            if self.tok == Tok::Eof {
                return err(self.pos, format!("{what} is not closed with `}}`"));
            }
            let (pos, key) = self.ident("a key or `}`")?;
            self.expect("=")?;
            let v = self.value()?;
            if out.iter().any(|(k, _, _)| *k == key) {
                return err(pos, format!("{what}: {key} given twice"));
            }
            out.push((key, pos, v));
        }
        Ok(out)
    }

    /// `@name(args)`, `@db.VarChar(n)`; `marker` is `@` or `@@`.
    fn attr(&mut self, marker: &str) -> Result<Attr> {
        let pos = self.expect(marker)?;
        let (_, mut name) = self.ident("an attribute name")?;
        while self.eat(".")? {
            name = format!("{name}.{}", self.ident("a name after `.`")?.1);
        }
        let args = if self.is("(") { self.args()? } else { Args { pos, ..Default::default() } };
        Ok(Attr { pos, name, args })
    }

    fn model(&mut self, pos: Pos, name: String) -> Result<ModelDecl> {
        self.expect("{")?;
        let (mut members, mut blocks) = (vec![], vec![]);
        let mut last_line = 0;
        loop {
            if self.eat("}")? {
                break;
            }
            if self.tok == Tok::Eof {
                return err(pos, format!("model {name} is not closed with `}}`"));
            }
            let start = self.pos;
            if start.line == last_line {
                return err(start, format!("model {name}: one field or attribute per line"));
            }
            let what = if self.is("@@") {
                blocks.push(self.attr("@@")?);
                "an attribute"
            } else {
                let (mpos, mname) = self.ident("a field name, `@@` or `}`")?;
                let (tpos, tname) = self.ident("a type")?;
                let targs = if self.is("(") { self.args()? } else { Args { pos: tpos, ..Default::default() } };
                let list = self.eat("[]")?;
                let optional = self.eat("?")?;
                let ty = TypeRef { pos: tpos, name: tname, args: targs, list, optional };
                let mut attrs = vec![];
                while self.is("@") && self.pos.line == start.line {
                    attrs.push(self.attr("@")?);
                }
                members.push(Member { pos: mpos, name: mname, ty, attrs });
                "a field"
            };
            // The Prisma formatter reads line by line: a split line falls apart on format.
            if self.prev_end.line != start.line || self.is("@") {
                return err(
                    start,
                    format!("model {name}: {what} must be on one line (put a long trigger body in a `function` block)"),
                );
            }
            last_line = start.line;
        }
        Ok(ModelDecl { pos, name, members, blocks })
    }

    fn enum_block(&mut self, pos: Pos, name: String) -> Result<EnumDecl> {
        self.expect("{")?;
        let (mut values, mut blocks) = (vec![], vec![]);
        let mut last_line = 0;
        loop {
            if self.eat("}")? {
                break;
            }
            if self.tok == Tok::Eof {
                return err(pos, format!("enum {name} is not closed with `}}`"));
            }
            let start = self.pos;
            if start.line == last_line {
                return err(start, format!("enum {name}: one value or attribute per line"));
            }
            if self.is("@@") {
                blocks.push(self.attr("@@")?);
            } else {
                let (vpos, value) = self.ident("an enum value, `@@` or `}`")?;
                let mut attrs = vec![];
                while self.is("@") && self.pos.line == start.line {
                    attrs.push(self.attr("@")?);
                }
                values.push((vpos, value, attrs));
            }
            if self.prev_end.line != start.line || self.is("@") {
                return err(start, format!("enum {name}: a value or attribute must be on one line"));
            }
            last_line = start.line;
        }
        Ok(EnumDecl { pos, name, values, blocks })
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
        let mut probe = self.lexer.clone();
        Ok(matches!(probe.next()?.1, Tok::Punct(":")))
    }

    /// `{ key: value, ... }` inside attribute arguments (storage parameters).
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
            if !self.eat(",")? {
                break;
            }
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
            (p, Tok::Text(_)) if self.in_model => {
                err(p, "\"\"\" strings only go in top-level blocks; put the SQL in a `function` block or use \"...\"")
            }
            (_, Tok::Text(s)) => Ok(Value::Str(s)),
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
        Tok::Str(_) | Tok::Text(_) => "a string".into(),
        Tok::Num(n) => format!("number {n}"),
        Tok::Punct(p) => format!("`{p}`"),
        Tok::Eof => "end of file".into(),
    }
}

pub fn parse(source: &str) -> Result<Vec<Item>> {
    let mut lexer = Lexer { chars: source.chars().peekable(), rest: source, pos: Pos { line: 1, col: 1 } };
    let (pos, tok) = lexer.next()?;
    let end = lexer.pos;
    let mut p = Parser { lexer, tok, pos, end, prev_end: Pos::default(), in_model: false };
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
            datasource db {
              provider   = "postgresql"
              extensions = [pg_trgm, uuid_ossp(map: "uuid-ossp")]
            }

            generator client {
              provider = "prisma-client-js"
            }

            model User {
              id    BigInt  @id @default(autoincrement())
              email String? @unique @db.VarChar(254)
              posts Post[]

              @@index([email(sort: Desc), sql("lower(email)")], where: raw("id > 0"), with: { m: 16 })
              @@map("users")
            }

            function f {
            returns = trigger
            body    = """
              BEGIN
                RETURN NEW;
              END;
            """
            }

            import "x.toml"
            "#,
        )
        .unwrap();
        assert_eq!(items.len(), 4);
        let Item::Datasource { props, .. } = &items[0] else { panic!() };
        assert_eq!(props[0].2, Value::Str("postgresql".into()));
        let Item::Model(m) = &items[1] else { panic!() };
        assert_eq!(m.members[1].ty.name, "String");
        assert!(m.members[1].ty.optional && !m.members[1].ty.list);
        assert_eq!(m.members[1].attrs[1].name, "db.VarChar");
        assert!(m.members[2].ty.list);
        assert_eq!(m.blocks[0].name, "index");
        assert_eq!(m.blocks[0].args.named.len(), 2);
        assert_eq!(m.blocks[1].name, "map");
        let Item::Function { props, .. } = &items[2] else { panic!() };
        assert_eq!(props[1].2, Value::Str("BEGIN\n  RETURN NEW;\nEND;".into()));
    }

    #[test]
    fn reports_positions() {
        let e = parse("model User {\n  id: BigInt\n}").unwrap_err();
        assert_eq!((e.pos.line, e.pos.col), (2, 5));
        assert!(e.msg.contains("expected a type"), "{}", e.msg);
        let e = parse("model X {\n  a String @default(\"oops)\n}").unwrap_err();
        assert!(e.msg.contains("unterminated"));
    }

    #[test]
    fn enforces_the_formatter_rules() {
        // one attribute per line
        let e = parse("model A {\n  id Int @id\n  @@trigger(t, after: [insert],\n    function: f)\n}").unwrap_err();
        assert_eq!(e.pos.line, 3);
        assert!(e.msg.contains("must be on one line"), "{}", e.msg);
        let e = parse("model A {\n  id Int\n    @id\n}").unwrap_err();
        assert!(e.msg.contains("a field must be on one line"), "{}", e.msg);
        let e = parse("model A {\n  id Int @id  b Int\n}").unwrap_err();
        assert!(e.msg.contains("one field or attribute per line"), "{}", e.msg);
        // """ only in top-level blocks
        let e = parse("model A {\n  id Int @id\n  @@trigger(t, before: [update], body: \"\"\"BEGIN RETURN NEW; END;\"\"\")\n}")
            .unwrap_err();
        assert!(e.msg.contains("only go in top-level blocks"), "{}", e.msg);
        assert!(parse("function f {\nbody = \"\"\"\nBEGIN RETURN NEW; END;\n\"\"\"\n}").is_ok());
    }
}
