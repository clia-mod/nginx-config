use combine::{eof, many, many1, Parser};
use combine::{choice, position};
use combine::combinator::{opaque, no_partial, FnOpaque};
use combine::error::StreamError;
use combine::easy::Error;

use ast::{self, Main, Directive, Item};
use error::ParseError;
use helpers::{semi, ident, text, string};
use position::Pos;
use tokenizer::{TokenStream, Token};
use value::Value;

use access;
use core;
use gzip;
use headers;
use proxy;
use rewrite;
use log;
use real_ip;


pub enum Code {
    Redirect(u32),
    Normal(u32),
}

pub fn bool<'a>() -> impl Parser<Output=bool, Input=TokenStream<'a>> {
    choice((
        ident("on").map(|_| true),
        ident("off").map(|_| false),
    ))
}

pub fn value<'a>() -> impl Parser<Output=Value, Input=TokenStream<'a>> {
    (position(), string())
    .and_then(|(p, v)| Value::parse(p, v))
}

pub fn worker_processes<'a>()
    -> impl Parser<Output=Item, Input=TokenStream<'a>>
{
    use ast::WorkerProcesses;
    ident("worker_processes")
    .with(choice((
        ident("auto").map(|_| WorkerProcesses::Auto),
        string().and_then(|s| s.value.parse().map(WorkerProcesses::Exact)),
    )))
    .skip(semi())
    .map(Item::WorkerProcesses)
}

pub fn server_name<'a>() -> impl Parser<Output=Item, Input=TokenStream<'a>> {
    use ast::ServerName::*;
    ident("server_name")
    .with(many1(
        string().map(|t| {
            if t.value.starts_with("~") {
                Regex(t.value[1..].to_string())
            } else if t.value.starts_with("*.") {
                StarSuffix(t.value[2..].to_string())
            } else if t.value.ends_with(".*") {
                StarPrefix(t.value[..t.value.len()-2].to_string())
            } else if t.value.starts_with(".") {
                Suffix(t.value[1..].to_string())
            } else {
                Exact(t.value.to_string())
            }
        })
    ))
    .skip(semi())
    .map(Item::ServerName)
}


pub fn map<'a>() -> impl Parser<Output=Item, Input=TokenStream<'a>> {
    use tokenizer::Kind::{BlockStart, BlockEnd};
    use helpers::kind;
    enum Tok {
        Hostnames,
        Volatile,
        Pattern(String, Value),
        Default(Value),
        Include(String),
    }
    ident("map")
    .with(value())
    .and(string().and_then(|t| {
        let ch1 = t.value.chars().nth(0).unwrap_or(' ');
        let ch2 = t.value.chars().nth(1).unwrap_or(' ');
        if ch1 == '$' && matches!(ch2, 'a'...'z' | 'A'...'Z' | '_') &&
            t.value[2..].chars()
            .all(|x| matches!(x, 'a'...'z' | 'A'...'Z' | '0'...'9' | '_'))
        {
            Ok(t.value[1..].to_string())
        } else {
            Err(Error::unexpected_message("invalid variable"))
        }
    }))
    .skip(kind(BlockStart))
    .and(many(choice((
        ident("hostnames").map(|_| Tok::Hostnames),
        ident("volatile").map(|_| Tok::Volatile),
        ident("default").with(value()).map(|v| Tok::Default(v)),
        ident("include").with(raw()).map(|v| Tok::Include(v)),
        raw().and(value()).map(|(s, v)| Tok::Pattern(s, v)),
    )).skip(semi())))
    .skip(kind(BlockEnd))
    .map(|((expression, variable), vec): ((_, _), Vec<Tok>)| {
        let mut res = ::ast::Map {
            variable, expression,
            default: None,
            hostnames: false,
            volatile: false,
            includes: Vec::new(),
            patterns: Vec::new(),
        };
        for val in vec {
            match val {
                Tok::Hostnames => res.hostnames = true,
                Tok::Volatile => res.volatile = true,
                Tok::Default(v) => res.default = Some(v),
                Tok::Include(path) => res.includes.push(path),
                Tok::Pattern(x, targ) => {
                    use ast::MapPattern::*;
                    let mut s = &x[..];
                    if s.starts_with('~') {
                        res.patterns.push((Regex(s[1..].to_string()), targ));
                        continue;
                    } else if s.starts_with('\\') {
                        s = &s[1..];
                    }
                    let pat = if res.hostnames {
                        if s.starts_with("*.") {
                            StarSuffix(s[2..].to_string())
                        } else if s.ends_with(".*") {
                            StarPrefix(s[..s.len()-2].to_string())
                        } else if s.starts_with(".") {
                            Suffix(s[1..].to_string())
                        } else {
                            Exact(s.to_string())
                        }
                    } else {
                        Exact(s.to_string())
                    };
                    res.patterns.push((pat, targ));
                }
            }
        }
        Item::Map(res)
    })
}

pub fn block<'a>()
    -> FnOpaque<TokenStream<'a>, ((Pos, Pos), Vec<Directive>)>
{
    use tokenizer::Kind::{BlockStart, BlockEnd};
    use helpers::kind;
    opaque(|f| {
        f(&mut no_partial((
                position(),
                kind(BlockStart)
                    .with(many(directive()))
                    .skip(kind(BlockEnd)),
                position(),
        ))
        .map(|(s, dirs, e)| ((s, e), dirs)))
    })
}

// A string that forbids variables
pub fn raw<'a>() -> impl Parser<Output=String, Input=TokenStream<'a>> {
    // TODO(tailhook) unquote single and double quotes
    // error on variables?
    string().and_then(|t| Ok::<_, Error<_, _>>(t.value.to_string()))
}

pub fn location<'a>() -> impl Parser<Output=Item, Input=TokenStream<'a>> {
    use ast::LocationPattern::*;
    ident("location").with(choice((
        text("=").with(raw().map(Exact)),
        text("^~").with(raw().map(FinalPrefix)),
        text("~").with(raw().map(Regex)),
        text("~*").with(raw().map(RegexInsensitive)),
        raw()
            .map(|v| if v.starts_with('*') {
                Named(v)
            } else {
                Prefix(v)
            }),
    ))).and(block())
    .map(|(pattern, (position, directives))| {
        Item::Location(ast::Location { pattern, position, directives })
    })
}

impl Code {
    pub fn parse<'x, 'y>(code_str: &'x str)
        -> Result<Code, Error<Token<'y>, Token<'y>>>
    {
        let code = code_str.parse::<u32>()?;
        match code {
            301 | 302 | 303 | 307 | 308 => Ok(Code::Redirect(code)),
            200...599 => Ok(Code::Normal(code)),
            _ => return Err(Error::unexpected_message(
                format!("invalid response code {}", code))),
        }
    }
    pub fn as_code(&self) -> u32 {
        match *self {
            Code::Redirect(code) => code,
            Code::Normal(code) => code,
        }
    }
}


pub fn try_files<'a>() -> impl Parser<Output=Item, Input=TokenStream<'a>> {
    use ast::TryFilesLastOption::*;
    use ast::Item::TryFiles;
    use value::Item::*;

    ident("try_files")
    .with(many1(value()))
    .skip(semi())
    .and_then(|mut v: Vec<_>| -> Result<_, Error<_, _>> {
        let last = v.pop().unwrap();
        let last = match &last.data[..] {
            [Literal(x)] if x.starts_with("=") => {
                Code(self::Code::parse(&x[1..])?.as_code())
            }
            [Literal(x)] if x.starts_with("@") => {
                NamedLocation(x[1..].to_string())
            }
            _ => Uri(last.clone()),
        };
        Ok(TryFiles(::ast::TryFiles {
            options: v,
            last_option: last,
        }))
    })
}


pub fn openresty<'a>() -> impl Parser<Output=Item, Input=TokenStream<'a>> {
    use ast::Item::*;
    choice((
        ident("rewrite_by_lua_file").with(value()).skip(semi())
            .map(Item::RewriteByLuaFile),
        ident("balancer_by_lua_file").with(value()).skip(semi())
            .map(BalancerByLuaFile),
        ident("access_by_lua_file").with(value()).skip(semi())
            .map(AccessByLuaFile),
        ident("header_filter_by_lua_file").with(value()).skip(semi())
            .map(HeaderFilterByLuaFile),
        ident("content_by_lua_file").with(value()).skip(semi())
            .map(ContentByLuaFile),
        ident("body_filter_by_lua_file").with(value()).skip(semi())
            .map(BodyFilterByLuaFile),
        ident("log_by_lua_file").with(value()).skip(semi())
            .map(LogByLuaFile),
        ident("lua_need_request_body").with(value()).skip(semi())
            .map(LuaNeedRequestBody),
        ident("ssl_certificate_by_lua_file").with(value()).skip(semi())
            .map(SslCertificateByLuaFile),
        ident("ssl_session_fetch_by_lua_file").with(value()).skip(semi())
            .map(SslSessionFetchByLuaFile),
        ident("ssl_session_store_by_lua_file").with(value()).skip(semi())
            .map(SslSessionStoreByLuaFile),
    ))
}

pub fn directive<'a>() -> impl Parser<Output=Directive, Input=TokenStream<'a>>
{
    position()
    .and(choice((
        ident("daemon").with(bool()).skip(semi())
            .map(Item::Daemon),
        ident("master_process").with(bool()).skip(semi())
            .map(Item::MasterProcess),
        worker_processes(),
        ident("http").with(block())
            .map(|(position, directives)| ast::Http { position, directives })
            .map(Item::Http),
        ident("server").with(block())
            .map(|(position, directives)| ast::Server { position, directives })
            .map(Item::Server),
        rewrite::directives(),
        try_files(),
        ident("include").with(value()).skip(semi()).map(Item::Include),
        ident("ssl_certificate").with(value()).skip(semi())
            .map(Item::SslCertificate),
        ident("ssl_certificate_key").with(value()).skip(semi())
            .map(Item::SslCertificateKey),
        location(),
        headers::directives(),
        server_name(),
        map(),
        ident("client_max_body_size").with(value()).skip(semi())
            .map(Item::ClientMaxBodySize),
        proxy::directives(),
        gzip::directives(),
        core::directives(),
        access::directives(),
        log::directives(),
        real_ip::directives(),
        openresty(),
        // it's own module
        ident("empty_gif").skip(semi()).map(|_| Item::EmptyGif),
        ident("index").with(many(value())).skip(semi())
            .map(Item::Index),
    )))
    .map(|(pos, dir)| Directive {
        position: pos,
        item: dir,
    })
}


/// Parses a piece of config in "main" context (i.e. top-level)
///
/// Currently, this is the same as parse_directives (except wraps everyting
/// to a `Main` struct), but we expect to
/// add validation/context checks in this function.
pub fn parse_main(s: &str) -> Result<Main, ParseError> {
    parse_directives(s).map(|directives| Main { directives })
}

/// Parses a piece of config from arbitrary context
///
/// This implies no validation of what context directives belong to.
pub fn parse_directives(s: &str) -> Result<Vec<Directive>, ParseError> {
    let mut tokens = TokenStream::new(s);
    let (doc, _) = many1(directive())
        .skip(eof())
        .parse_stream(&mut tokens)
        .map_err(|e| e.into_inner().error)?;
    Ok(doc)
}

use std::path::{Path, PathBuf};
use std::fs;
use glob::glob;
use std::collections::HashMap;

/// Parse a file on disk and also expand `include` directives using globbing.
///
/// Includes with variable references are left untouched. Included files are
/// processed recursively using their directory as a base for relative paths.
pub fn parse_directives_from_file<P: AsRef<Path>>(path: P)
    -> Result<Vec<Directive>, ::failure::Error>
{
    let path = path.as_ref();
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let data = fs::read_to_string(path)?;
    let mut directives = parse_directives(&data)?;
    let mut vars: HashMap<String, String> = HashMap::new();
    expand_includes(&mut directives, base, Some(path), &mut vars)?;
    Ok(directives)
}

/// Convenience helper to parse a main config file and expand includes.
pub fn parse_main_from_file<P: AsRef<Path>>(path: P)
    -> Result<Main, ::failure::Error>
{
    let dirs = parse_directives_from_file(path)?;
    Ok(Main { directives: dirs })
}

fn value_to_path(v: &Value) -> Option<String> {
    use value::Item;
    let mut s = String::new();
    for item in &v.data {
        match item {
            Item::Literal(x) => s.push_str(&x),
            Item::Variable(_) => return None,
        }
    }
    Some(s)
}

fn resolve_value_with_vars(v: &Value, vars: &HashMap<String, String>) -> Option<String> {
    use value::Item;
    let mut s = String::new();
    for item in &v.data {
        match item {
            Item::Literal(x) => s.push_str(x),
            Item::Variable(name) => {
                if let Some(val) = vars.get(name) {
                    s.push_str(val);
                } else {
                    return None;
                }
            }
        }
    }
    Some(s)
}

fn expand_includes(dirs: &mut Vec<Directive>, base: &Path, current_file: Option<&Path>, vars: &mut HashMap<String, String>)
    -> Result<(), ::failure::Error>
{
    let mut i = 0;
    while i < dirs.len() {
        // Update variable map if this directive is a `set` in the current scope.
        match dirs[i].item {
            ast::Item::Set { ref variable, ref value } => {
                if let Some(resolved) = resolve_value_with_vars(value, vars) {
                    vars.insert(variable.clone(), resolved);
                }
            }
            _ => {}
        }

        // Recurse into blocks first — blocks create a new local variable scope (clone vars)
        {
            use ast::Item::*;
            match dirs[i].item {
                Http(ref mut h) => { let mut subvars = vars.clone(); expand_includes(&mut h.directives, base, current_file, &mut subvars)?; }
                Server(ref mut s) => { let mut subvars = vars.clone(); expand_includes(&mut s.directives, base, current_file, &mut subvars)?; }
                Location(ref mut l) => { let mut subvars = vars.clone(); expand_includes(&mut l.directives, base, current_file, &mut subvars)?; }
                If(ref mut iff) => { let mut subvars = vars.clone(); expand_includes(&mut iff.directives, base, current_file, &mut subvars)?; }
                LimitExcept(ref mut le) => { let mut subvars = vars.clone(); expand_includes(&mut le.directives, base, current_file, &mut subvars)?; }
                _ => {}
            }
        }
        // Now handle include directive itself
        match dirs[i].item.clone() {
            ast::Item::Include(ref v) => {
                // try to resolve include path using vars; support mixed literal+variables
                if let Some(pat) = resolve_value_with_vars(v, vars).or_else(|| value_to_path(v)) {
                    // Interpret pattern relative to base
                    let full_pat = base.join(&pat).to_string_lossy().into_owned();
                    let mut inserted = Vec::new();
                    for entry in glob(&full_pat)? {
                        if let Ok(path) = entry {
                            // don't include the file that contains the include
                            if let Some(cur) = current_file {
                                if fs::canonicalize(&path)? == fs::canonicalize(cur)? {
                                    continue;
                                }
                            }
                            if path.is_file() {
                                let data = fs::read_to_string(&path)?;
                                let mut inc_dirs = parse_directives(&data)?;
                                // recursively expand includes within included file
                                if let Some(dirp) = path.parent() {
                                    // included file shares current variable scope (included content acts as if inserted here)
                                    expand_includes(&mut inc_dirs, dirp, Some(&path), vars)?;
                                }
                                inserted.append(&mut inc_dirs);
                            }
                        }
                    }
                    if !inserted.is_empty() {
                        // replace the include directive with inserted ones
                        dirs.splice(i..=i, inserted.into_iter());
                        // do not increment i, process the newly inserted
                        continue;
                    }
                }
                // If pattern contains unresolved variables or no files matched - leave as-is
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}
