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
//! [`normalize`]; anything else fails with exit code 1.
//!
//! Two more modes for splitting a production file into child modules
//! (CR-P2-08 production part, batch U review):
//!
//!   tokcmp items <file.rs>
//!       one line per top-level item: first line (outer attributes and doc
//!       comments included), last line, kind, name.
//!
//!   tokcmp split <old_file.rs> <new_parent.rs> <name>=<child.rs>...
//!       flattens the new parent (each `mod name;` replaced by the child
//!       file's items, minus the child's leading `use super::*;`; the
//!       parent's `use name::*;` glob of each child dropped) and compares
//!       the old file's top-level items with the flattened ones as a
//!       multiset of normalized token strings. Visibility is normalized on
//!       both sides: `pub(crate)`, `pub(super)` and `pub(in ..)` count as
//!       private, on items, fields and impl items; `pub` is compared as
//!       is, so the public API cannot change unnoticed. Inner attributes of
//!       a child file must be doc comments (the module's `//!` text) and
//!       are reported, not compared. The same normalization is applied to
//!       the raw source lines (comments included): old lines vs parent +
//!       child lines, minus the lines the split itself adds; differing
//!       lines are printed. Exit code 1 when the item multisets differ.
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

fn legacy_main(args: Vec<String>) {
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("items") if args.len() == 3 => items_main(&args[2]),
        Some("split") if args.len() >= 5 => split_main(&args[2], &args[3], &args[4..]),
        _ => legacy_main(args),
    }
}

fn parse(path: &str) -> (String, syn::File) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("read {path}: {e}");
        exit(2)
    });
    let file = syn::parse_file(&text).unwrap_or_else(|e| {
        eprintln!("parse {path}: {e}");
        exit(2)
    });
    (text, file)
}

fn item_kind_name(item: &syn::Item) -> (&'static str, String) {
    use syn::Item;
    match item {
        Item::Const(i) => ("const", i.ident.to_string()),
        Item::Enum(i) => ("enum", i.ident.to_string()),
        Item::ExternCrate(i) => ("extern crate", i.ident.to_string()),
        Item::Fn(i) => ("fn", i.sig.ident.to_string()),
        Item::ForeignMod(_) => ("extern", String::new()),
        Item::Impl(i) => {
            let ty = i.self_ty.to_token_stream().to_string();
            match &i.trait_ {
                Some((_, path, _)) => ("impl", format!("{} for {ty}", path.to_token_stream())),
                None => ("impl", ty),
            }
        }
        Item::Macro(i) => (
            "macro",
            i.ident
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_else(|| i.mac.path.to_token_stream().to_string()),
        ),
        Item::Mod(i) => ("mod", i.ident.to_string()),
        Item::Static(i) => ("static", i.ident.to_string()),
        Item::Struct(i) => ("struct", i.ident.to_string()),
        Item::Trait(i) => ("trait", i.ident.to_string()),
        Item::TraitAlias(i) => ("trait alias", i.ident.to_string()),
        Item::Type(i) => ("type", i.ident.to_string()),
        Item::Union(i) => ("union", i.ident.to_string()),
        Item::Use(i) => ("use", i.tree.to_token_stream().to_string()),
        _ => ("other", String::new()),
    }
}

fn items_main(path: &str) {
    use syn::spanned::Spanned;
    let (_, file) = parse(path);
    for item in &file.items {
        let span = item.span();
        let (kind, name) = item_kind_name(item);
        println!("{} {} {kind} {name}", span.start().line, span.end().line);
    }
}

fn norm_vis(v: &mut syn::Visibility) {
    if matches!(v, syn::Visibility::Restricted(_)) {
        *v = syn::Visibility::Inherited;
    }
}

fn norm_fields(fields: &mut syn::Fields) {
    for f in fields.iter_mut() {
        norm_vis(&mut f.vis);
    }
}

fn norm_item(item: &mut syn::Item) {
    use syn::Item;
    match item {
        Item::Const(i) => norm_vis(&mut i.vis),
        Item::Enum(i) => norm_vis(&mut i.vis),
        Item::ExternCrate(i) => norm_vis(&mut i.vis),
        Item::Fn(i) => norm_vis(&mut i.vis),
        Item::Impl(i) => {
            for it in i.items.iter_mut() {
                match it {
                    syn::ImplItem::Const(c) => norm_vis(&mut c.vis),
                    syn::ImplItem::Fn(f) => norm_vis(&mut f.vis),
                    syn::ImplItem::Type(t) => norm_vis(&mut t.vis),
                    _ => {}
                }
            }
        }
        Item::Mod(i) => {
            norm_vis(&mut i.vis);
            if let Some((_, items)) = i.content.as_mut() {
                items.iter_mut().for_each(norm_item);
            }
        }
        Item::Static(i) => norm_vis(&mut i.vis),
        Item::Struct(i) => {
            norm_vis(&mut i.vis);
            norm_fields(&mut i.fields);
        }
        Item::Trait(i) => norm_vis(&mut i.vis),
        Item::TraitAlias(i) => norm_vis(&mut i.vis),
        Item::Type(i) => norm_vis(&mut i.vis),
        Item::Union(i) => {
            norm_vis(&mut i.vis);
            for f in i.fields.named.iter_mut() {
                norm_vis(&mut f.vis);
            }
        }
        Item::Use(i) => norm_vis(&mut i.vis),
        _ => {}
    }
}

/// `use <name>::*;` with any visibility and no attributes.
fn is_glob_of(item: &syn::Item, name: &str) -> bool {
    if let syn::Item::Use(u) = item {
        if !u.attrs.is_empty() {
            return false;
        }
        if let syn::UseTree::Path(p) = &u.tree {
            return p.ident == name && matches!(*p.tree, syn::UseTree::Glob(_));
        }
    }
    false
}

fn is_use_super_glob(item: &syn::Item) -> bool {
    is_glob_of(item, "super")
        && matches!(item, syn::Item::Use(u) if matches!(u.vis, syn::Visibility::Inherited))
}

/// The source-line form of the same visibility normalization.
fn norm_line(line: &str) -> String {
    let mut l = line.trim_end().to_string();
    for pat in ["pub(crate) ", "pub(super) "] {
        l = l.replace(pat, "");
    }
    l
}

fn split_main(old_path: &str, new_parent_path: &str, children: &[String]) {
    use std::collections::BTreeMap;
    let (old_text, old) = parse(old_path);
    let (parent_text, parent) = parse(new_parent_path);
    let mut child_files: Vec<(String, String, syn::File)> = Vec::new();
    for spec in children {
        let (name, path) = spec.split_once('=').unwrap_or_else(|| {
            eprintln!("child spec must be <name>=<file.rs>: {spec}");
            exit(2)
        });
        let (text, file) = parse(path);
        child_files.push((name.to_string(), text, file));
    }

    // Flatten the new parent.
    let names: Vec<&str> = child_files.iter().map(|(n, _, _)| n.as_str()).collect();
    let mut flat: Vec<syn::Item> = Vec::new();
    let mut seen_mods = Vec::new();
    for item in &parent.items {
        if names.iter().any(|n| is_glob_of(item, n)) {
            continue;
        }
        if let syn::Item::Mod(m) = item {
            if m.content.is_none() {
                if let Some((name, _, child)) =
                    child_files.iter().find(|(n, _, _)| m.ident == n.as_str())
                {
                    for attr in &child.attrs {
                        if !attr.path().is_ident("doc") {
                            eprintln!("child {name}: non-doc inner attribute {}", attr.to_token_stream());
                            exit(1);
                        }
                    }
                    println!("child {name}: {} doc line(s) of module docs (not compared)", child.attrs.len());
                    let mut items = child.items.iter();
                    match items.next() {
                        Some(first) if is_use_super_glob(first) => {}
                        _ => {
                            eprintln!("child {name}: first item must be `use super::*;`");
                            exit(1);
                        }
                    }
                    flat.extend(items.cloned());
                    seen_mods.push(name.clone());
                    continue;
                }
            }
        }
        flat.push(item.clone());
    }
    for (name, _, _) in &child_files {
        if !seen_mods.contains(name) {
            eprintln!("parent has no `mod {name};`");
            exit(1);
        }
    }

    let key = |item: &syn::Item| {
        let mut it = item.clone();
        norm_item(&mut it);
        normalize(it.to_token_stream()).to_string()
    };
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    for item in &old.items {
        let k = key(item);
        let (kind, name) = item_kind_name(item);
        labels.entry(k.clone()).or_insert(format!("{kind} {name}"));
        *counts.entry(k).or_default() += 1;
    }
    for item in &flat {
        let k = key(item);
        let (kind, name) = item_kind_name(item);
        labels.entry(k.clone()).or_insert(format!("{kind} {name}"));
        *counts.entry(k).or_default() -= 1;
    }
    let attrs_ok = tokens(&old.attrs) == tokens(&parent.attrs);
    let mut only_old = Vec::new();
    let mut only_new = Vec::new();
    for (k, c) in &counts {
        if *c > 0 {
            only_old.push(labels[k].clone());
        } else if *c < 0 {
            only_new.push(labels[k].clone());
        }
    }
    println!("items: old {} / new {} after flattening", old.items.len(), flat.len());
    println!("parent inner attributes identical: {attrs_ok}");
    println!("item multiset identical (visibility normalized): {}", only_old.is_empty() && only_new.is_empty());
    for l in &only_old {
        println!("  only in old: {l}");
    }
    for l in &only_new {
        println!("  only in new: {l}");
    }

    // Line-level check, comments included.
    let mut lines: BTreeMap<String, i64> = BTreeMap::new();
    for l in old_text.lines() {
        if !l.trim().is_empty() {
            *lines.entry(norm_line(l)).or_default() += 1;
        }
    }
    let added_by_split = |l: &str| {
        let t = l.trim();
        names.iter().any(|n| {
            t == format!("mod {n};")
                || t.ends_with(&format!("use {n}::*;")) && t.split_whitespace().count() <= 3
        })
    };
    for l in parent_text.lines() {
        if !l.trim().is_empty() && !added_by_split(l) {
            *lines.entry(norm_line(l)).or_default() -= 1;
        }
    }
    for (name, text, _) in &child_files {
        let mut header = true;
        for l in text.lines() {
            let t = l.trim();
            if header && (t.starts_with("//!") || t.is_empty() || t == "use super::*;") {
                continue;
            }
            header = false;
            if !t.is_empty() {
                *lines.entry(norm_line(l)).or_default() -= 1;
            }
        }
        let _ = name;
    }
    let line_diffs: Vec<_> = lines.iter().filter(|(_, c)| **c != 0).collect();
    println!("source lines identical (visibility normalized, split lines excluded): {}", line_diffs.is_empty());
    for (l, c) in line_diffs.iter().take(40) {
        println!("  {} {l}", if **c > 0 { "old+" } else { "new+" });
    }
    if line_diffs.len() > 40 {
        println!("  ... {} more", line_diffs.len() - 40);
    }
    if !(attrs_ok && only_old.is_empty() && only_new.is_empty()) {
        exit(1);
    }
}
