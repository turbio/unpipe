use rnix::ast::{BinOp, BinOpKind, Select};
use rowan::ast::AstNode;
use rnix::{NodeOrToken, Root, SyntaxKind::*, SyntaxNode};
use std::io::{self, Read, Write};
use std::process::ExitCode;

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    // ident, literal, paren, set, list, ... anything that can sit bare in
    // argument position
    Atom,
    // `f x`, fine as a function but needs parens as an argument
    App,
    // operators, lambdas, let/if/with, needs parens everywhere
    Other,
}

fn pipe_kind(node: &SyntaxNode) -> Option<BinOpKind> {
    match BinOp::cast(node.clone())?.operator()? {
        k @ (BinOpKind::PipeRight | BinOpKind::PipeLeft) => Some(k),
        _ => None,
    }
}

fn shape(node: &SyntaxNode) -> Shape {
    match node.kind() {
        NODE_IDENT | NODE_LITERAL | NODE_STRING | NODE_PAREN | NODE_ATTR_SET | NODE_LIST
        | NODE_PATH_ABS | NODE_PATH_REL | NODE_PATH_HOME | NODE_PATH_SEARCH | NODE_CUR_POS
        | NODE_LEGACY_LET => Shape::Atom,
        // `a.b or c` is technically fine bare but lets not find out
        NODE_SELECT => match Select::cast(node.clone()).and_then(|s| s.default_expr()) {
            Some(_) => Shape::Other,
            None => Shape::Atom,
        },
        NODE_APPLY => Shape::App,
        // a pipe turns into an application once we're done with it
        NODE_BIN_OP if pipe_kind(node).is_some() => Shape::App,
        _ => Shape::Other,
    }
}

fn render(node: &SyntaxNode) -> String {
    if pipe_kind(node).is_some() {
        return render_pipe(node);
    }
    let mut out = String::new();
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Token(t) => out.push_str(t.text()),
            NodeOrToken::Node(n) => out.push_str(&render(&n)),
        }
    }
    out
}

fn render_pipe(node: &SyntaxNode) -> String {
    let bin = BinOp::cast(node.clone()).unwrap();
    let lhs = bin.lhs().unwrap();
    let rhs = bin.rhs().unwrap();
    let (func, arg) = match pipe_kind(node).unwrap() {
        BinOpKind::PipeRight => (rhs, lhs),
        _ => (lhs, rhs),
    };

    let trivia: Vec<_> = node
        .children_with_tokens()
        .filter_map(|c| c.into_token())
        .filter(|t| t.kind().is_trivia())
        .collect();

    // if theres a comment hiding in there keep all of it to be safe
    let sep = if trivia.iter().any(|t| t.kind() == TOKEN_COMMENT) {
        trivia.iter().map(|t| t.text()).collect::<String>()
    } else {
        " ".to_string()
    };

    let f = render(func.syntax());
    let f = match shape(func.syntax()) {
        Shape::Atom | Shape::App => f,
        Shape::Other => format!("({f})"),
    };
    let a = render(arg.syntax());
    let a = match shape(arg.syntax()) {
        Shape::Atom => a,
        _ => format!("({a})"),
    };
    format!("{f}{sep}{a}")
}

fn has_pipe(node: &SyntaxNode) -> bool {
    node.descendants().any(|n| pipe_kind(&n).is_some())
}

fn unpipe(src: &str) -> Result<String, String> {
    let parse = Root::parse(src);
    if let Some(err) = parse.errors().first() {
        return Err(format!("parse error: {err}"));
    }

    let root = parse.syntax();
    let out = render(&root);

    let check = Root::parse(&out);

    if let Some(err) = check.errors().first() {
        return Err(format!("internal error: output doesn't parse ({err}), refusing to write:\n{out}"));
    }

    // ya never know
    if has_pipe(&check.syntax()) {
        return Err("internal error: output still has pipes, refusing to write".into());
    }

    Ok(out)
}

const USAGE: &str = "\
usage: unpipe [--check] [FILES...]

rewrites nix files so they don't use the experimental pipe operators
(|> and <|). with no files (or `-`) reads stdin and writes stdout,
otherwise rewrites each file in place.

  --check     don't write anything, exit 1 if any file would change
  -h, --help  this
  --version   version";

fn main() -> ExitCode {
    let mut check = false;
    let mut files = vec![];
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--version" => {
                println!("unpipe {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--check" => check = true,
            "-" => {}
            a if a.starts_with('-') => {
                eprintln!("unknown flag {a}\n{USAGE}");
                return ExitCode::FAILURE;
            }
            _ => files.push(arg),
        }
    }

    let mut failed = false;
    if files.is_empty() {
        let mut src = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut src) {
            eprintln!("<stdin>: {e}");
            return ExitCode::FAILURE;
        }
        match unpipe(&src) {
            Ok(out) if check => failed = out != src,
            Ok(out) => {
                let _ = io::stdout().write_all(out.as_bytes());
            }
            Err(e) => {
                eprintln!("<stdin>: {e}");
                failed = true;
            }
        }
    }
    for path in files {
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{path}: {e}");
                failed = true;
                continue;
            }
        };
        match unpipe(&src) {
            Ok(out) if out == src => {}
            Ok(_) if check => {
                eprintln!("{path}: has pipes");
                failed = true;
            }
            Ok(out) => {
                // same dance as nixfmt: write a temp then rename so we never
                // leave a half written file around
                let tmp = format!("{path}.unpipe-tmp");
                let res = std::fs::write(&tmp, &out).and_then(|_| std::fs::rename(&tmp, &path));
                if let Err(e) = res {
                    eprintln!("{path}: {e}");
                    let _ = std::fs::remove_file(&tmp);
                    failed = true;
                }
            }
            Err(e) => {
                eprintln!("{path}: {e}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::unpipe;

    fn t(input: &str, expected: &str) {
        assert_eq!(unpipe(input).unwrap(), expected, "input: {input}");
    }

    #[test]
    fn basics() {
        t("1 |> f", "f 1");
        t("f <| 1", "f 1");
        t("a |> f |> g", "g (f a)");
        t("f <| g <| a", "f (g a)");
        t("a |> f x |> g y z |> h", "h (g y z (f x a))");
        t("1 + 1 |> builtins.toString", "builtins.toString (1 + 1)");
        t("builtins.toString <| 1 + 1 |> foo", "builtins.toString (foo (1 + 1))");
        t("c <| b <| a", "c (b a)");
    }

    #[test]
    fn parens() {
        t("a |> (n: n + 1)", "(n: n + 1) a");
        t("-1 |> f", "f (-1)");
        t("f a |> g", "g (f a)");
        t("a |> f |> g { }", "g { } (f a)");
        t("a |> x.y", "x.y a");
        t("a |> x.y or z", "(x.y or z) a");
        t("[ 1 2 ] |> map f", "map f [ 1 2 ]");
        t("a ? b |> f", "f (a ? b)");
    }

    #[test]
    fn nested_and_untouched() {
        t("{ foo = a |> b; bar = a |> b |> c; }", "{ foo = b a; bar = c (b a); }");
        t("\"${a |> f}\"", "\"${f a}\"");
        t("let x = 1; in x", "let x = 1; in x");
        t("if a |> f then b else c", "if f a then b else c");
        t("(a |> f) |> g", "g (f a)");
    }

    #[test]
    fn whitespace_and_comments() {
        t("foo\n  |> map f\n  |> filter g", "filter g (map f foo)");
        t("foo # hi\n|> f", "f # hi\n foo");
        t("# leading\nfoo |> f\n", "# leading\nf foo\n");
    }
}
