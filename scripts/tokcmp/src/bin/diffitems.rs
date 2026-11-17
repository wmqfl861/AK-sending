//! Diagnosis companion of `tokcmp`: print every item that differs (after the
//! same rustfmt-inert canonicalization) between an old inline module and its
//! new module file. usage: diffitems <old_parent.rs> <new_module_file.rs> <module_name>
use quote::ToTokens;
/// Canonicalize rustfmt-inert differences, nothing else:
/// - a trailing `,` inside a delimited group that has at least one other
///   top-level `,` (2+ elements: tuples, params, args, arrays, arms);
/// - a single trailing `,` in a CALL argument list, i.e. a parenthesized
///   group directly after a non-keyword identifier (`f(x,)` == `f(x)`);
///   a bare `(x,)` 1-tuple keeps its comma and is still reported;
/// - string / byte-string literals compared by VALUE: a `\` line
///   continuation swallows the newline and the next line's leading
///   whitespace, so re-indenting continuation lines changes the source
///   text of the token but not the string.
fn normalize(ts: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    use proc_macro2::{Group, Literal, TokenTree};
    const KEYWORDS: &[&str] = &[
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
        "use", "where", "while", "async", "await", "dyn", "yield",
    ];
    let mut out: Vec<TokenTree> = Vec::new();
    for tt in ts {
        let call_like = matches!(out.last(), Some(TokenTree::Ident(i)) if !KEYWORDS.contains(&i.to_string().as_str()));
        match tt {
            TokenTree::Group(g) => {
                let inner: Vec<TokenTree> = normalize(g.stream()).into_iter().collect();
                let commas = inner
                    .iter()
                    .filter(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ','))
                    .count();
                let trailing =
                    matches!(inner.last(), Some(TokenTree::Punct(p)) if p.as_char() == ',');
                let call = call_like && g.delimiter() == proc_macro2::Delimiter::Parenthesis;
                let inner: Vec<TokenTree> = if trailing && (commas >= 2 || call) {
                    inner[..inner.len() - 1].to_vec()
                } else {
                    inner
                };
                let mut ng = Group::new(g.delimiter(), inner.into_iter().collect());
                ng.set_span(g.span());
                out.push(TokenTree::Group(ng));
            }
            TokenTree::Literal(l) => {
                let canon = match syn::parse_str::<syn::Lit>(&l.to_string()) {
                    Ok(syn::Lit::Str(s)) if s.suffix().is_empty() => Literal::string(&s.value()),
                    Ok(syn::Lit::ByteStr(b)) if b.suffix().is_empty() => {
                        Literal::byte_string(&b.value())
                    }
                    _ => l,
                };
                out.push(TokenTree::Literal(canon));
            }
            other => out.push(other),
        }
    }
    out.into_iter().collect()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let old = syn::parse_file(&std::fs::read_to_string(&a[1]).unwrap()).unwrap();
    let new = syn::parse_file(&std::fs::read_to_string(&a[2]).unwrap()).unwrap();
    let name = &a[3];
    let items = old
        .items
        .iter()
        .find_map(|i| match i {
            syn::Item::Mod(m) if m.ident == name.as_str() => {
                m.content.as_ref().map(|c| c.1.clone())
            }
            _ => None,
        })
        .unwrap();
    let mut diffs = 0;
    for (i, (x, y)) in items.iter().zip(new.items.iter()).enumerate() {
        let (xs, ys) = (
            normalize(x.to_token_stream()).to_string(),
            normalize(y.to_token_stream()).to_string(),
        );
        if xs != ys {
            diffs += 1;
            // find first differing char
            let p = xs
                .chars()
                .zip(ys.chars())
                .take_while(|(c, d)| c == d)
                .count();
            let lo = p.saturating_sub(120);
            println!(
                "item #{i} differs at char {p}:\n OLD: {}\n NEW: {}",
                xs.chars().skip(lo).take(300).collect::<String>(),
                ys.chars().skip(lo).take(300).collect::<String>()
            );
        }
    }
    println!("differing items: {diffs}");
}
