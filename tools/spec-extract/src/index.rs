//! syn-based structural index of the workspace's public API surface
//! (40-spec-verification.md §3.1).
//!
//! We parse every `.rs` file with `syn::parse_file` and emit a flat list of
//! [`Item`] records — one per struct / enum / const / static / fn / type-alias
//! / trait — carrying exactly the structural facts the drift checker compares:
//! field and variant *names in declaration order*, const values, and normalized
//! fn signatures. Order is part of the contract: `fields` and `variants` are
//! NEVER sorted (§3.1 compares order).
//!
//! LIMITATION (documented per §3.3): this is a parse-only pass. It never
//! compiles, runs `cargo metadata`, or expands macros. Items produced by a
//! macro invocation (`macro_rules!`, derive/attribute proc-macros) are
//! therefore INVISIBLE to the index — only items written literally in source
//! are seen. The drift checker only makes claims about literal source, so this
//! is a deliberate boundary, not a bug.

use std::path::Path;

use quote::ToTokens;
use serde::Serialize;

/// The kind of a top-level item the index records.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Struct,
    Enum,
    Const,
    Static,
    Fn,
    TypeAlias,
    Trait,
}

#[derive(Serialize)]
pub struct Variant {
    pub name: String,
    /// Named-variant field names in declaration order; tuple variants -> "0",
    /// "1", ...; unit variants -> empty.
    pub fields: Vec<String>,
}

/// One indexed item. Field docs mirror 40 §3.1's schema exactly.
#[derive(Serialize)]
pub struct Item {
    /// Repo-relative, forward slashes, e.g. "crates/photonic-core/src/document.rs".
    pub file: String,
    /// 1-based line of the item's first *declaration* token (the `struct` /
    /// `enum` / `const` / `fn` / … keyword), NOT its leading doc comment.
    pub line: u32,
    /// Crate name, e.g. "photonic-core".
    pub krate: String,
    /// Underscored crate name + file-derived mods + inline `mod` nesting,
    /// e.g. "photonic_core::document".
    pub module_path: String,
    pub name: String,
    pub kind: Kind,
    /// "pub" | "pub(crate)" | "pub(super)" | "pub(in ...)" | "private".
    pub visibility: String,
    /// Struct field names in DECLARATION ORDER; tuple structs -> "0","1",...;
    /// empty for enums/consts/fns/etc.
    pub fields: Vec<String>,
    /// Enum variants in DECLARATION ORDER; empty otherwise.
    pub variants: Vec<Variant>,
    /// Const/Static value, `quote!(#expr)` whitespace-collapsed, e.g. "4".
    pub value: Option<String>,
    /// Fn signature "fn name(a: A) -> R" (generics/where included, body
    /// excluded), quote-rendered + whitespace-collapsed.
    pub signature: Option<String>,
    /// Rendered `#[cfg(...)]` predicates, e.g. ["feature = \"video\""].
    pub cfg: Vec<String>,
}

/// The top-level JSON envelope.
#[derive(Serialize)]
pub struct Index {
    pub schema: u32,
    pub items: Vec<Item>,
}

impl Index {
    pub fn new(mut items: Vec<Item>) -> Self {
        // Deterministic output: sort by (file, line). Stable sort keeps the
        // rare same-line ties in source order.
        items.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
        Index { schema: 1, items }
    }
}

/// Collapse all runs of whitespace to single spaces and trim — used to
/// normalize quote-rendered values and signatures into a canonical form.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn line_of(span: proc_macro2::Span) -> u32 {
    // `span-locations` is enabled, so the fallback spans that `syn::parse_file`
    // produces carry real 1-based line numbers.
    span.start().line as u32
}

/// Render a `#[cfg(...)]` predicate body, e.g. `feature = "video"` or `unix`.
fn render_cfg(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for a in attrs {
        if a.path().is_ident("cfg") {
            if let syn::Meta::List(list) = &a.meta {
                out.push(collapse_ws(&list.tokens.to_string()));
            }
        }
    }
    out
}

fn render_visibility(vis: &syn::Visibility) -> String {
    match vis {
        syn::Visibility::Public(_) => "pub".to_string(),
        syn::Visibility::Inherited => "private".to_string(),
        syn::Visibility::Restricted(r) => {
            let path = r.path.to_token_stream().to_string();
            let path = collapse_ws(&path);
            if r.in_token.is_some() {
                format!("pub(in {path})")
            } else {
                format!("pub({path})")
            }
        }
    }
}

/// Field names of a `syn::Fields` in declaration order.
fn field_names(fields: &syn::Fields) -> Vec<String> {
    match fields {
        syn::Fields::Named(named) => named
            .named
            .iter()
            .map(|f| f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default())
            .collect(),
        syn::Fields::Unnamed(unnamed) => {
            (0..unnamed.unnamed.len()).map(|i| i.to_string()).collect()
        }
        syn::Fields::Unit => Vec::new(),
    }
}

/// A file's crate name and module prefix, derived from its repo-relative path.
struct FileScope {
    krate: String,
    /// e.g. "photonic_core::document" (no trailing inline-mod segments yet).
    module_prefix: String,
}

/// Derive `{ krate, module_prefix }` from a repo-relative path such as
/// `crates/photonic-core/src/foo/bar.rs`.
///
/// - `mod.rs`, `lib.rs`, `main.rs` add no path segment.
/// - `foo.rs` adds `foo`; `foo/bar.rs` adds `foo::bar`.
/// - The prefix is the underscored crate name followed by those segments.
fn file_scope(rel: &str) -> FileScope {
    let krate = rel
        .strip_prefix("crates/")
        .and_then(|r| r.split('/').next())
        .unwrap_or("")
        .to_string();
    let krate_us = krate.replace('-', "_");

    // Everything after the crate's `src/` or `tests/` root.
    let after = if let Some(i) = rel.find("/src/") {
        &rel[i + "/src/".len()..]
    } else if let Some(i) = rel.find("/tests/") {
        &rel[i + "/tests/".len()..]
    } else {
        rel
    };
    let after = after.strip_suffix(".rs").unwrap_or(after);

    let mut segs = vec![krate_us];
    for part in after.split('/') {
        if part.is_empty() || part == "mod" || part == "lib" || part == "main" {
            continue;
        }
        segs.push(part.to_string());
    }

    FileScope {
        krate,
        module_prefix: segs.join("::"),
    }
}

/// Walk a slice of items, emitting index entries and recursing into inline
/// `mod` blocks. `inherited_cfg` carries `#[cfg(...)]` from enclosing modules
/// so items inside a `#[cfg(test)] mod tests { … }` are tagged with it.
fn collect_items(
    items: &[syn::Item],
    file: &str,
    krate: &str,
    module_path: &str,
    inherited_cfg: &[String],
    out: &mut Vec<Item>,
) {
    for item in items {
        match item {
            syn::Item::Struct(s) => {
                out.push(make_item(
                    file,
                    krate,
                    module_path,
                    line_of(s.struct_token.span),
                    s.ident.to_string(),
                    Kind::Struct,
                    render_visibility(&s.vis),
                    field_names(&s.fields),
                    Vec::new(),
                    None,
                    None,
                    merge_cfg(inherited_cfg, &s.attrs),
                ));
            }
            syn::Item::Enum(e) => {
                let variants = e
                    .variants
                    .iter()
                    .map(|v| Variant {
                        name: v.ident.to_string(),
                        fields: field_names(&v.fields),
                    })
                    .collect();
                out.push(make_item(
                    file,
                    krate,
                    module_path,
                    line_of(e.enum_token.span),
                    e.ident.to_string(),
                    Kind::Enum,
                    render_visibility(&e.vis),
                    Vec::new(),
                    variants,
                    None,
                    None,
                    merge_cfg(inherited_cfg, &e.attrs),
                ));
            }
            syn::Item::Const(c) => {
                out.push(make_item(
                    file,
                    krate,
                    module_path,
                    line_of(c.const_token.span),
                    c.ident.to_string(),
                    Kind::Const,
                    render_visibility(&c.vis),
                    Vec::new(),
                    Vec::new(),
                    Some(collapse_ws(&c.expr.to_token_stream().to_string())),
                    None,
                    merge_cfg(inherited_cfg, &c.attrs),
                ));
            }
            syn::Item::Static(st) => {
                out.push(make_item(
                    file,
                    krate,
                    module_path,
                    line_of(st.static_token.span),
                    st.ident.to_string(),
                    Kind::Static,
                    render_visibility(&st.vis),
                    Vec::new(),
                    Vec::new(),
                    Some(collapse_ws(&st.expr.to_token_stream().to_string())),
                    None,
                    merge_cfg(inherited_cfg, &st.attrs),
                ));
            }
            syn::Item::Fn(f) => {
                out.push(make_item(
                    file,
                    krate,
                    module_path,
                    line_of(f.sig.fn_token.span),
                    f.sig.ident.to_string(),
                    Kind::Fn,
                    render_visibility(&f.vis),
                    Vec::new(),
                    Vec::new(),
                    None,
                    Some(collapse_ws(&f.sig.to_token_stream().to_string())),
                    merge_cfg(inherited_cfg, &f.attrs),
                ));
            }
            syn::Item::Type(t) => {
                out.push(make_item(
                    file,
                    krate,
                    module_path,
                    line_of(t.type_token.span),
                    t.ident.to_string(),
                    Kind::TypeAlias,
                    render_visibility(&t.vis),
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    merge_cfg(inherited_cfg, &t.attrs),
                ));
            }
            syn::Item::Trait(tr) => {
                out.push(make_item(
                    file,
                    krate,
                    module_path,
                    line_of(tr.trait_token.span),
                    tr.ident.to_string(),
                    Kind::Trait,
                    render_visibility(&tr.vis),
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    merge_cfg(inherited_cfg, &tr.attrs),
                ));
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    let nested = format!("{module_path}::{}", m.ident);
                    let cfg = merge_cfg(inherited_cfg, &m.attrs);
                    collect_items(inner, file, krate, &nested, &cfg, out);
                }
            }
            _ => {}
        }
    }
}

/// Combine module-inherited cfg predicates with an item's own, in that order.
fn merge_cfg(inherited: &[String], attrs: &[syn::Attribute]) -> Vec<String> {
    let mut cfg = inherited.to_vec();
    cfg.extend(render_cfg(attrs));
    cfg
}

#[allow(clippy::too_many_arguments)]
fn make_item(
    file: &str,
    krate: &str,
    module_path: &str,
    line: u32,
    name: String,
    kind: Kind,
    visibility: String,
    fields: Vec<String>,
    variants: Vec<Variant>,
    value: Option<String>,
    signature: Option<String>,
    cfg: Vec<String>,
) -> Item {
    Item {
        file: file.to_string(),
        line,
        krate: krate.to_string(),
        module_path: module_path.to_string(),
        name,
        kind,
        visibility,
        fields,
        variants,
        value,
        signature,
        cfg,
    }
}

/// Parse one `.rs` file and append its items to `out`. A parse failure is a
/// HARD ERROR (§3): we return it so the caller can name file + span and abort,
/// never silently skip — a skipped file is how the checker starts lying.
pub fn index_file(root: &Path, path: &Path, out: &mut Vec<Item>) -> Result<(), String> {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let src = std::fs::read_to_string(path).map_err(|e| format!("{rel}: read error: {e}"))?;
    let file = syn::parse_file(&src).map_err(|e| {
        let span = e.span();
        format!("{rel}:{}: parse error: {e}", span.start().line)
    })?;
    let scope = file_scope(&rel);
    collect_items(
        &file.items,
        &rel,
        &scope.krate,
        &scope.module_prefix,
        &[],
        out,
    );
    Ok(())
}

/// Parse ONE item from a source fragment (used by `--stdin-fragment`, which the
/// anchored-block checker in §3.1 pipes each `spec-source` block through).
pub fn index_fragment(src: &str) -> Result<Item, String> {
    let item: syn::Item = syn::parse_str(src).map_err(|e| format!("fragment parse error: {e}"))?;
    let mut out = Vec::new();
    collect_items(&[item], "<stdin>", "", "", &[], &mut out);
    out.into_iter()
        .next()
        .ok_or_else(|| "fragment produced no indexable item".to_string())
}
