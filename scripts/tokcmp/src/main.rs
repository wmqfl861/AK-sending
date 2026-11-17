//! Token-level pure-move check for an inline module moved to its own file
//! (M4, CR-P2-08; review 2026-11-17). Standalone dev tool, not a workspace
//! member and never shipped; its lock file pins the syn / quote /
//! proc-macro2 / unicode-ident versions the root Cargo.lock already uses.
//!
//! usage:
//!   cargo run --release --manifest-path scripts/tokcmp/Cargo.toml --bin tokcmp -- \
//!       <old_parent.rs> <new_parent.rs> <new_module_file.rs> <module_name>
//!
//! Compares (1) the old inline module's items with the new module file's
//! inner attributes + items, and (2) the old parent with the inline module
//! replaced by `mod name;` (same outer attributes) against the new parent.
//! Comments are not tokens: pair this with the byte-level check
//! (rustfmt of the 4-space de-indented old body == the new file). The only
//! differences tolerated are the rustfmt-inert ones listed on
//! [`normalize`]; anything else fails with exit code 1. `diffitems` (same
//! arguments minus the new parent: <old_parent> <new_module_file> <name>)
//! prints the first differing items for diagnosis.
use quote::ToTokens;
use std::process::exit;

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

fn tokens<T: ToTokens>(items: &[T]) -> String {
    let mut ts = proc_macro2::TokenStream::new();
    for item in items {
        item.to_tokens(&mut ts);
    }
    normalize(ts).to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: tokcmp <old_parent> <new_parent> <new_module_file> <module>");
        exit(2);
    }
    let read =
        |p: &str| syn::parse_file(&std::fs::read_to_string(p).expect("read")).expect("parse");
    let old_parent = read(&args[1]);
    let new_parent = read(&args[2]);
    let new_module = read(&args[3]);
    let name = &args[4];

    let mut found = None;
    let mut old_rewritten = old_parent.clone();
    for item in old_rewritten.items.iter_mut() {
        if let syn::Item::Mod(m) = item {
            if m.ident == name.as_str() {
                let (_, items) = m.content.take().expect("old module must be inline");
                m.semi = Some(Default::default());
                found = Some(items);
            }
        }
    }
    let old_items = found.unwrap_or_else(|| {
        eprintln!("inline module {name} not found in old parent");
        exit(1)
    });
    // The old inline module's inner attributes (#![...]) live in m.attrs as
    // inner-style attrs; report them so the move can be judged.
    let old_body = tokens(&old_items);
    let new_body = format!("{}{}", tokens(&new_module.attrs), tokens(&new_module.items));
    let parent_old = format!(
        "{}{}",
        tokens(&old_rewritten.attrs),
        tokens(&old_rewritten.items)
    );
    let parent_new = format!("{}{}", tokens(&new_parent.attrs), tokens(&new_parent.items));
    let body_ok = old_body == new_body;
    let parent_ok = parent_old == parent_new;
    println!(
        "module items: old {} / new {}",
        old_items.len(),
        new_module.items.len()
    );
    println!("module body tokens identical: {body_ok}");
    println!("parent tokens identical (inline module -> `mod {name};`): {parent_ok}");
    if !(body_ok && parent_ok) {
        exit(1);
    }
}
