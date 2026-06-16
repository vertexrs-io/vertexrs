use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{
    Expr, ExprClosure, ExprPath, Ident, Pat, ReturnType, Token, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
    visit::Visit,
};

// ── Input parsing ─────────────────────────────────────────────────────────────

struct NodeInput {
    name: Ident,
    expr: Expr,
}

impl Parse for NodeInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        input.parse::<Token![=]>()?;
        let expr: Expr = input.parse()?;
        Ok(NodeInput { name, expr })
    }
}

// ── Recognise `receiver.row(|arg| body)` / `receiver.col(|arg| body)` ─────────

#[derive(Debug, PartialEq, Clone, Copy)]
enum AccessMode {
    Row,
    Col,
}

struct NodeCall<'a> {
    receiver: &'a Expr,
    mode: AccessMode,
    closure: &'a ExprClosure,
}

/// Extracts the `receiver`, mode, and closure from a top-level method-call
/// expression.  Returns `None` if the expression does not match the pattern.
fn extract_node_call(expr: &Expr) -> Option<NodeCall<'_>> {
    let Expr::MethodCall(call) = expr else {
        return None;
    };
    let mode = match call.method.to_string().as_str() {
        "row" => AccessMode::Row,
        "col" => AccessMode::Col,
        _ => return None,
    };
    if call.args.len() != 1 {
        return None;
    }
    let Expr::Closure(closure) = &call.args[0] else {
        return None;
    };
    Some(NodeCall {
        receiver: &call.receiver,
        mode,
        closure,
    })
}

/// Extracts the single identifier from a receiver expression, e.g. `price` from
/// `price`.  Returns `None` for complex receivers.
fn receiver_ident(expr: &Expr) -> Option<&Ident> {
    let Expr::Path(ExprPath {
        qself: None, path, ..
    }) = expr
    else {
        return None;
    };
    if path.segments.len() == 1 {
        Some(&path.segments[0].ident)
    } else {
        None
    }
}

/// Returns `(ident, type_annotation)` for each closure argument.
///
/// - `Pat::Ident(x)`         → `(x, None)`    — untyped (single-column mode)
/// - `Pat::Type(x: T)` → `(x, Some(T))` — typed   (multi-column Frame mode)
///
/// Arguments with other patterns (destructuring, `_`, etc.) are skipped.
fn closure_typed_args(closure: &ExprClosure) -> Vec<(Ident, Option<Type>)> {
    closure
        .inputs
        .iter()
        .filter_map(|pat| match pat {
            Pat::Ident(p) => Some((p.ident.clone(), None)),
            Pat::Type(pt) => {
                if let Pat::Ident(inner) = pt.pat.as_ref() {
                    Some((inner.ident.clone(), Some((*pt.ty).clone())))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect()
}

// ── Collect extra node dependencies from a row-closure body ───────────────────
//
// Any bare identifier in the body (other than the closure arg) is treated as a
// potential upstream node reference.  The macro shadows it with
// `let dep = dep.data[__vtx_i];` in the emitted kernel, so if `dep` is not a
// `Node<T>`, the code fails to compile with a clear field-not-found error.
//
// Nested closures are NOT walked: their arguments would appear as false deps.
// Multi-segment paths (`std::f64::MAX`) and call-position identifiers (`sort`,
// `filter`) are excluded by the two `visit_*` overrides below.

struct BodyDepCollector {
    excluded: Vec<String>,
    deps: Vec<String>,
}

impl<'ast> Visit<'ast> for BodyDepCollector {
    fn visit_expr_path(&mut self, node: &'ast ExprPath) {
        if node.qself.is_none() && node.path.segments.len() == 1 {
            let name = node.path.segments[0].ident.to_string();
            if !self.excluded.contains(&name) && !self.deps.contains(&name) {
                self.deps.push(name);
            }
        }
    }

    // Skip the function-position identifier in free-function calls so that
    // `f64::sqrt(dep)` only records `dep`, not `sqrt`.
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        for arg in &node.args {
            self.visit_expr(arg);
        }
    }

    // Do not recurse into nested closures — their bound variables are not deps.
    fn visit_expr_closure(&mut self, _: &'ast ExprClosure) {}
}

// ── Macro entry point ─────────────────────────────────────────────────────────

/// Returns `true` when the closure carries an explicit `-> bool` return-type
/// annotation.  Used by the `node!` macro to select between emitting a
/// `Node<T>` (numeric/primitive) and a `BoolNode` (boolean column).
fn is_bool_return(closure: &ExprClosure) -> bool {
    match &closure.output {
        ReturnType::Type(_, ty) => matches!(ty.as_ref(), Type::Path(p) if p.path.is_ident("bool")),
        ReturnType::Default => false,
    }
}

/// Defines a named DAG node computed from an upstream column node.
///
/// # Syntax
///
/// ```text
/// node!(name = source.row(|elem| expr))
/// node!(name = source.col(|col|  expr))
/// ```
///
/// **Row mode** — element-wise kernel lifted across the column:
/// ```text
/// node!(tax   = price.row(|x| x * 0.2));
/// node!(total = price.row(|x| x + tax));   // `tax` is another node
/// ```
///
/// **Col mode** — whole-column operation (may change length):
/// ```text
/// node!(sorted   = price.col(|c| c.sort()));
/// node!(filtered = price.col(|c| c.filter(|x| *x > 0.0)));
/// node!(tax_on_sorted = sorted.row(|x| x * 0.2));  // resumes after col op
/// ```
///
/// Source nodes (no upstream deps) are created with [`Node::from_data`].
#[proc_macro]
pub fn node(input: TokenStream) -> TokenStream {
    let NodeInput { name, expr } = parse_macro_input!(input as NodeInput);

    let node_name = name.to_string();
    let name_lit = syn::LitStr::new(&node_name, Span::call_site());

    let Some(NodeCall {
        receiver,
        mode,
        closure,
    }) = extract_node_call(&expr)
    else {
        return syn::Error::new_spanned(
            &expr,
            "node! expects `name = source.row(|x| expr)` or `name = source.col(|c| expr)`",
        )
        .to_compile_error()
        .into();
    };

    let Some(recv_ident) = receiver_ident(receiver) else {
        return syn::Error::new_spanned(
            receiver,
            "node! receiver must be a simple identifier (e.g. `price`, not an expression)",
        )
        .to_compile_error()
        .into();
    };

    let recv_name = recv_ident.to_string();
    let recv_lit = syn::LitStr::new(&recv_name, Span::call_site());
    let recv_ident_ts = recv_ident;

    let body = &closure.body;

    match mode {
        // ── Row mode ──────────────────────────────────────────────────────────
        //
        // `price.row(|x| x * 0.2)`  →  iterate over row indices, bind `x` to
        // `price.data[__vtx_i]`, shadow any other node refs the same way.
        //
        // Emitted code:
        //   let tax = Node::new_with_deps("tax", &["price"], (0..price.len())
        //       .map(|__vtx_i| { let x = price.data[__vtx_i]; body })
        //       .collect::<Vec<_>>());
        AccessMode::Row => {
            let typed_args = closure_typed_args(closure);
            let multi_col_mode = typed_args.iter().any(|(_, ty)| ty.is_some());

            if multi_col_mode {
                // ── Multi-column Frame row mode ──────────────────────────────
                // `frame.row(|price: f64, qty: i32| price * qty as f64)`
                //
                // Every argument must carry a type annotation in this mode.
                for (ident, ty) in &typed_args {
                    if ty.is_none() {
                        return syn::Error::new_spanned(
                            ident,
                            "node! multi-column row mode: all closure arguments must have type annotations",
                        )
                        .to_compile_error()
                        .into();
                    }
                }

                let col_binds = typed_args.iter().map(|(ident, ty)| {
                    let col_lit = syn::LitStr::new(&ident.to_string(), Span::call_site());
                    quote! {
                        let #ident = #recv_ident_ts
                            .get::<#ty>(#col_lit)
                            .unwrap_or_else(|| panic!("column '{}' not found in Frame", #col_lit))
                            [__vtx_i];
                    }
                });

                let arg_names: Vec<String> =
                    typed_args.iter().map(|(i, _)| i.to_string()).collect();
                let col_dep_lits: Vec<syn::LitStr> = arg_names
                    .iter()
                    .map(|n| syn::LitStr::new(n, Span::call_site()))
                    .collect();

                let mut dep_collector = BodyDepCollector {
                    excluded: arg_names,
                    deps: Vec::new(),
                };
                dep_collector.visit_expr(body);
                let extra_dep_lits: Vec<syn::LitStr> = dep_collector
                    .deps
                    .iter()
                    .filter(|d| d.as_str() != node_name)
                    .map(|d| syn::LitStr::new(d, Span::call_site()))
                    .collect();

                quote! {
                    let #name = Node::new_with_deps(
                        #name_lit,
                        &[#(#col_dep_lits,)* #(#extra_dep_lits),*],
                        (0..#recv_ident_ts.len())
                            .map(|__vtx_i| {
                                #(#col_binds)*
                                #body
                            })
                            .collect::<Vec<_>>(),
                    );
                }
                .into()
            } else {
                // ── Single-column row mode (existing path) ───────────────────
                let arg_name = typed_args.into_iter().next().map(|(i, _)| i.to_string());
                let excluded_name = arg_name.clone().unwrap_or_default();

                let mut dep_collector = BodyDepCollector {
                    excluded: vec![excluded_name],
                    deps: Vec::new(),
                };
                dep_collector.visit_expr(body);

                let extra_deps: Vec<Ident> = dep_collector
                    .deps
                    .iter()
                    .filter(|d| d.as_str() != node_name)
                    .map(|d| Ident::new(d, Span::call_site()))
                    .collect();

                let extra_dep_lits: Vec<syn::LitStr> = extra_deps
                    .iter()
                    .map(|i| syn::LitStr::new(&i.to_string(), Span::call_site()))
                    .collect();

                let arg_bind = arg_name.map(|a| {
                    let arg_ident = Ident::new(&a, Span::call_site());
                    quote! { let #arg_ident = #recv_ident_ts.data[__vtx_i]; }
                });

                let dep_binds = extra_deps.iter().map(|d| {
                    quote! { let #d = #d.data[__vtx_i]; }
                });

                // Propagate the explicit return type annotation (if any) to the
                // generated map closure so that diverging bodies (e.g. `panic!()`)
                // are typed correctly rather than being inferred as `!`.
                let map_return_hint = match &closure.output {
                    ReturnType::Type(arrow, ty) => quote! { #arrow #ty },
                    ReturnType::Default => quote! {},
                };

                if is_bool_return(closure) {
                    quote! {
                        let #name = BoolNode::new_with_deps(
                            #name_lit,
                            &[#recv_lit, #(#extra_dep_lits),*],
                            (0..#recv_ident_ts.len())
                                .map(|__vtx_i| #map_return_hint {
                                    #arg_bind
                                    #(#dep_binds)*
                                    #body
                                })
                                .collect::<Vec<bool>>(),
                        );
                    }
                    .into()
                } else {
                    quote! {
                        let #name = Node::new_with_deps(
                            #name_lit,
                            &[#recv_lit, #(#extra_dep_lits),*],
                            (0..#recv_ident_ts.len())
                                .map(|__vtx_i| #map_return_hint {
                                    #arg_bind
                                    #(#dep_binds)*
                                    #body
                                })
                                .collect::<Vec<_>>(),
                        );
                    }
                    .into()
                }
            }
        }

        AccessMode::Col => {
            let typed_args = closure_typed_args(closure);
            let multi_col_mode = typed_args.iter().any(|(_, ty)| ty.is_some());

            if multi_col_mode {
                // ── Named Frame column col mode ──────────────────────────────
                // `frame.col(|price: f64| price.sort())`
                if typed_args.len() != 1 {
                    return syn::Error::new_spanned(
                        &closure.body,
                        "node! Frame col mode expects exactly one typed argument",
                    )
                    .to_compile_error()
                    .into();
                }
                let (col_ident, col_ty) = typed_args.into_iter().next().unwrap();
                let col_ty = col_ty.unwrap();
                let col_lit = syn::LitStr::new(&col_ident.to_string(), Span::call_site());

                quote! {
                    let #name = Node::new_with_deps(
                        #name_lit,
                        &[#col_lit],
                        {
                            let #col_ident = ColRef {
                                data: #recv_ident_ts
                                    .get::<#col_ty>(#col_lit)
                                    .unwrap_or_else(|| panic!("column '{}' not found in Frame", #col_lit)),
                            };
                            #body
                        },
                    );
                }
                .into()
            } else {
                // ── Single-column col mode (existing path) ───────────────────
                let arg_name = typed_args.into_iter().next().map(|(i, _)| i.to_string());
                let excluded_name = arg_name.unwrap_or_default();

                let mut dep_collector = BodyDepCollector {
                    excluded: vec![excluded_name],
                    deps: Vec::new(),
                };
                dep_collector.visit_expr(body);

                let extra_dep_lits: Vec<syn::LitStr> = dep_collector
                    .deps
                    .iter()
                    .filter(|d| d.as_str() != node_name && d.as_str() != recv_name)
                    .map(|d| syn::LitStr::new(d, Span::call_site()))
                    .collect();

                quote! {
                    let #name = Node::new_with_deps(
                        #name_lit,
                        &[#recv_lit, #(#extra_dep_lits),*],
                        #expr,
                    );
                }
                .into()
            }
        }
    }
}

// ── pipeline! ────────────────────────────────────────────────────────────────

/// Per-node failure mode override declared via a trailing sigil on a `node!`
/// expression inside `pipeline!`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum NodeFailureOverride {
    /// No sigil — behave as before (panic propagates; existing tests unaffected).
    None,
    /// `?` sigil — soft failure: catch panic, write NA, push warning, continue.
    Soft,
    /// `!` sigil — hard failure: catch panic, return `PipelineError::KernelPanic`.
    Hard,
}

/// Parsed metadata for a `node!(…)` item inside `pipeline!`.
struct NodeItemMeta {
    /// Node name (the LHS of `name = expr`).
    name: Ident,
    /// Primary receiver identifier, used to compute NA fallback length.
    /// `None` only if the receiver expression is not a simple ident (compile
    /// error in `node!` anyway, so the pipeline will fail to compile).
    receiver_ident: Option<Ident>,
    /// Core expression: `node!` tokens without the sigil and without the
    /// `pure` option.  Suitable for passing directly to `vertexrs::node!(…)`.
    core_expr_tokens: proc_macro2::TokenStream,
    /// Per-node failure override.
    failure_override: NodeFailureOverride,
    /// When `false`, the incremental executor must always recompute this node.
    pure: bool,
}

/// Parses `name = expr[?|!][, pure = bool]` from the content of a `node!(…)`
/// declaration inside `pipeline!`.
fn parse_node_item_meta(input: ParseStream) -> syn::Result<NodeItemMeta> {
    let name: Ident = input.parse()?;
    input.parse::<Token![=]>()?;

    // Parse the expression.  `syn` will consume `?` as `Expr::Try` but will
    // leave a trailing `!` unconsumed (not a valid postfix operator in Rust).
    let expr: Expr = input.parse()?;

    // Detect sigil.
    let (core_expr, failure_override) = if let Expr::Try(try_expr) = expr {
        (*try_expr.expr, NodeFailureOverride::Soft)
    } else if input.peek(Token![!]) {
        input.parse::<Token![!]>()?;
        (expr, NodeFailureOverride::Hard)
    } else {
        (expr, NodeFailureOverride::None)
    };

    // Detect `, pure = <bool>`.
    let pure = if input.peek(Token![,]) {
        input.parse::<Token![,]>()?;
        let key: Ident = input.parse()?;
        if key != "pure" {
            return Err(syn::Error::new(
                key.span(),
                "expected `pure` key in node options (e.g. `pure = false`)",
            ));
        }
        input.parse::<Token![=]>()?;
        let val: syn::LitBool = input.parse()?;
        val.value
    } else {
        true
    };

    // Extract receiver ident for NA-fallback length.
    let receiver_ident =
        extract_node_call(&core_expr).and_then(|nc| receiver_ident(nc.receiver).cloned());

    let core_expr_tokens = quote! { #name = #core_expr };

    Ok(NodeItemMeta {
        name,
        receiver_ident,
        core_expr_tokens,
        failure_override,
        pure,
    })
}

/// Failure mode specified in a nested `pipeline!(name { settings { failure: Mode } ... })` block.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FailureModeKind {
    Soft,
    Hard,
    Isolate,
}

/// A source column declaration inside `pipeline!`.
struct PipelineSource {
    name: Ident,
    ty: Type,
}

/// An inline nested pipeline: `pipeline!(name { [settings { failure: Mode }] items... })`.
struct NestedPipelineItem {
    name: Ident,
    failure_mode: FailureModeKind,
    /// Raw tokens of the inner pipeline body (everything after optional settings).
    inner_tokens: proc_macro2::TokenStream,
}

/// An external sub-pipeline injection: `sub!(expr => out_name: out_type, ...)`.
struct SubItem {
    /// Expression that evaluates to a `Pipeline` value (e.g. `normaliser()`).
    expr: Expr,
    /// Declared output columns to extract from the sub-pipeline's output Frame.
    outputs: Vec<(Ident, Type)>,
}

/// An ordered pipeline item (everything except `source!` and `output!`).
enum OrderedItem {
    Node(Box<NodeItemMeta>),
    Nested(Box<NestedPipelineItem>),
    Sub(Box<SubItem>),
}

/// A single statement inside a `pipeline!` block.
enum PipelineItem {
    Source(Box<PipelineSource>),
    Ordered(OrderedItem),
    Output(Vec<Ident>),
}

struct PipelineDef {
    items: Vec<PipelineItem>,
}

/// Parse an optional `settings { failure: Mode }` block from the start of a
/// nested pipeline body.  If the next token is not `settings`, returns `Soft`.
fn try_parse_settings(input: syn::parse::ParseStream) -> syn::Result<FailureModeKind> {
    // Peek: is the next token the identifier `settings` followed by `{`?
    if input.peek(Ident) {
        let forked = input.fork();
        let kw: Ident = forked.parse()?;
        if kw == "settings" && forked.peek(syn::token::Brace) {
            let _: Ident = input.parse()?; // consume "settings"
            let settings_inner;
            syn::braced!(settings_inner in input);
            let key: Ident = settings_inner.parse()?;
            if key != "failure" {
                return Err(syn::Error::new(
                    key.span(),
                    "expected `failure` key in settings block",
                ));
            }
            settings_inner.parse::<Token![:]>()?;
            let mode: Ident = settings_inner.parse()?;
            return match mode.to_string().as_str() {
                "Soft" => Ok(FailureModeKind::Soft),
                "Hard" => Ok(FailureModeKind::Hard),
                "Isolate" => Ok(FailureModeKind::Isolate),
                other => Err(syn::Error::new(
                    mode.span(),
                    format!("unknown failure mode `{other}`; expected Soft, Hard, or Isolate"),
                )),
            };
        }
    }
    Ok(FailureModeKind::Soft)
}

impl Parse for PipelineDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut items = Vec::new();
        while !input.is_empty() {
            let mac_name: Ident = input.parse()?;
            input.parse::<Token![!]>()?;

            // Every pipeline statement uses parentheses: source!(...), pipeline!(...), sub!(...)
            let content;
            syn::parenthesized!(content in input);

            match mac_name.to_string().as_str() {
                "source" => {
                    loop {
                        let name: Ident = content.parse()?;
                        content.parse::<Token![:]>()?;
                        let ty: Type = content.parse()?;
                        items.push(PipelineItem::Source(Box::new(PipelineSource { name, ty })));
                        if content.is_empty() {
                            break;
                        }
                        content.parse::<Token![,]>()?;
                        if content.is_empty() {
                            break;
                        } // trailing comma
                    }
                }
                "node" => {
                    let meta = content.call(parse_node_item_meta)?;
                    items.push(PipelineItem::Ordered(OrderedItem::Node(Box::new(meta))));
                }
                "output" => {
                    let mut names = Vec::new();
                    while !content.is_empty() {
                        names.push(content.parse::<Ident>()?);
                        if content.is_empty() {
                            break;
                        }
                        content.parse::<Token![,]>()?;
                    }
                    items.push(PipelineItem::Output(names));
                }
                // ── Nested inline pipeline: pipeline!(name { [settings { }] items... }) ──
                "pipeline" => {
                    let name: Ident = content.parse()?;
                    let inner;
                    syn::braced!(inner in content);
                    let failure_mode = try_parse_settings(&inner)?;
                    let inner_tokens: proc_macro2::TokenStream = inner.parse()?;
                    items.push(PipelineItem::Ordered(OrderedItem::Nested(Box::new(
                        NestedPipelineItem {
                            name,
                            failure_mode,
                            inner_tokens,
                        },
                    ))));
                }
                // ── External sub-pipeline: sub!(expr => name: type, ...) ──────────────
                "sub" => {
                    let expr: Expr = content.parse()?;
                    content.parse::<Token![=>]>()?;
                    let mut outputs: Vec<(Ident, Type)> = Vec::new();
                    loop {
                        let out_name: Ident = content.parse()?;
                        content.parse::<Token![:]>()?;
                        let out_ty: Type = content.parse()?;
                        outputs.push((out_name, out_ty));
                        if content.is_empty() {
                            break;
                        }
                        content.parse::<Token![,]>()?;
                        if content.is_empty() {
                            break;
                        } // trailing comma
                    }
                    items.push(PipelineItem::Ordered(OrderedItem::Sub(Box::new(SubItem {
                        expr,
                        outputs,
                    }))));
                }
                other => {
                    return Err(syn::Error::new(
                        mac_name.span(),
                        format!(
                            "unknown pipeline statement `{other}!`; \
                             expected `source!`, `node!`, `pipeline!`, `sub!`, or `output!`"
                        ),
                    ));
                }
            }

            if input.peek(Token![;]) {
                input.parse::<Token![;]>()?;
            }
        }
        Ok(PipelineDef { items })
    }
}

// ── Kernel fusion helpers ─────────────────────────────────────────────────────
//
// These helpers implement the compile-time fusion pass for `pipeline!`. They
// replace the old `for item in &ordered` loop with a two-phase pass:
// (1) `group_fusable` identifies linear chains of pure row-mode nodes, and
// (2) the codegen loop calls `emit_fused_chain` for each `Fused` group.

/// Classification of a pipeline run after the fusion pass.
///
/// `Single`, `Nested`, and `Sub` variants emit exactly as today.
/// `Fused` emits as one loop containing all node computations in the chain.
enum FusionGroup<'a> {
    /// A single node emitted by the existing per-node codegen path.
    Single(&'a NodeItemMeta),
    /// Two or more consecutive pure, single-arg, untyped, row-mode nodes
    /// forming a linear chain (len >= 2). Emitted as one fused loop.
    Fused(Vec<&'a NodeItemMeta>),
    /// A nested inline pipeline — emitted exactly as before.
    Nested(&'a NestedPipelineItem),
    /// An external sub-pipeline — emitted exactly as before.
    Sub(&'a SubItem),
}

/// Returns `true` iff `meta` is eligible to participate in a fusion chain.
///
/// All three conditions must hold:
/// - `meta.pure == true` (impure nodes always break the chain)
/// - `meta.failure_override == NodeFailureOverride::None` (sigil nodes wrap
///   the kernel in `catch_unwind`, which precludes inlining into a shared loop)
/// - `meta.core_expr_tokens` re-parses as a single-arg, untyped, row-mode call
///   (`receiver.row(|x| body)`) — col-mode, multi-arg typed, and bool-return
///   closures are excluded per ADR-0004 (fusion is columnar-path only)
fn is_row_fusable(meta: &NodeItemMeta) -> bool {
    if !meta.pure || meta.failure_override != NodeFailureOverride::None {
        return false;
    }
    let ts = meta.core_expr_tokens.clone();
    let Ok(node_input) = syn::parse2::<NodeInput>(ts) else {
        return false;
    };
    let Some(NodeCall { mode, closure, .. }) = extract_node_call(&node_input.expr) else {
        return false;
    };
    if mode != AccessMode::Row {
        return false;
    }
    // bool-return closures produce BoolNode, not Node<T: ArrowNativeType>.
    if is_bool_return(closure) {
        return false;
    }
    let args = closure_typed_args(closure);
    args.len() == 1 && args[0].1.is_none()
}

/// Returns `true` iff `consumer` can be chain-linked directly after `producer`.
///
/// The only requirement: `consumer`'s primary receiver ident equals
/// `producer`'s name — i.e. `consumer` reads its primary input from `producer`.
fn chains_from(producer: &NodeItemMeta, consumer: &NodeItemMeta) -> bool {
    consumer
        .receiver_ident
        .as_ref()
        .map(|r| r == &producer.name)
        .unwrap_or(false)
}

/// Groups `ordered` items into `FusionGroup`s ready for code generation.
///
/// Algorithm:
/// 1. Build a fan-out count map (node name → number of nodes that use it as
///    their primary receiver).
/// 2. Walk items in declaration order, accumulating a running fusable chain.
///    - A chain extends when the candidate is fusable, directly `chains_from`
///      the previous node, AND the previous node's fan-out count is exactly 1.
/// 3. On a chain break (non-fusable node, fan-out > 1, or non-Node item), flush
///    the running chain as `Single` (len 1) or `Fused` (len >= 2).
/// 4. Non-Node items force a flush and are inserted as `Nested` / `Sub` variants.
fn group_fusable<'a>(ordered: &'a [OrderedItem]) -> Vec<FusionGroup<'a>> {
    // Count how many nodes use each name as their primary receiver.
    let mut fanout: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for item in ordered {
        if let OrderedItem::Node(meta) = item
            && let Some(recv) = &meta.receiver_ident
        {
            *fanout.entry(recv.to_string()).or_insert(0) += 1;
        }
    }

    let mut groups: Vec<FusionGroup<'a>> = Vec::new();
    let mut current_chain: Vec<&'a NodeItemMeta> = Vec::new();

    // Flush `current_chain` into `groups` and reset it.
    macro_rules! flush {
        () => {
            match current_chain.len() {
                0 => {}
                1 => {
                    groups.push(FusionGroup::Single(current_chain[0]));
                    current_chain.clear();
                }
                _ => {
                    groups.push(FusionGroup::Fused(std::mem::take(&mut current_chain)));
                }
            }
        };
    }

    for item in ordered {
        match item {
            OrderedItem::Node(meta) => {
                let fusable = is_row_fusable(meta);
                let can_extend = fusable
                    && current_chain.last().is_some_and(|prev| {
                        chains_from(prev, meta)
                            && fanout.get(&prev.name.to_string()).copied().unwrap_or(0) == 1
                    });

                if can_extend {
                    current_chain.push(meta);
                } else {
                    flush!();
                    if fusable {
                        current_chain.push(meta);
                    } else {
                        groups.push(FusionGroup::Single(meta));
                    }
                }
            }
            OrderedItem::Nested(n) => {
                flush!();
                groups.push(FusionGroup::Nested(n));
            }
            OrderedItem::Sub(s) => {
                flush!();
                groups.push(FusionGroup::Sub(s));
            }
        }
    }
    flush!();

    groups
}

/// Emits a single fused kernel block for a `FusionGroup::Fused` chain.
///
/// The emitted code:
/// 1. Pre-allocates one `Vec::with_capacity(n)` per node, where `n` is the
///    first node's receiver length.
/// 2. Runs a single `for __vtx_i in 0..receiver.len()` loop that computes each
///    node's expression in sequence, pipelining intermediate values in registers.
/// 3. Binds each accumulator as `vertexrs::Node::new_with_deps(...)` so any
///    downstream non-fused node that references an intermediate name compiles.
///
/// Extra body deps (identifiers in the closure body other than the arg) are
/// resolved inside each loop step: earlier chain-node deps use the corresponding
/// `__vtx_*_val` variable; external deps use `dep.data[__vtx_i]`.
fn emit_fused_chain(chain: &[&NodeItemMeta]) -> proc_macro2::TokenStream {
    debug_assert!(chain.len() >= 2, "fused chain must have at least 2 nodes");

    // Map each chain node's name to its index for intra-chain dep resolution.
    let idx_of: std::collections::HashMap<String, usize> = chain
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name.to_string(), i))
        .collect();

    // The first node's receiver is the loop-bound source.
    let first_receiver = chain[0]
        .receiver_ident
        .as_ref()
        .expect("fusable node always has a receiver_ident");

    // Accumulator variable names and intermediate value variable names.
    let data_vars: Vec<Ident> = chain
        .iter()
        .map(|m| Ident::new(&format!("__vtx_{}_data", m.name), Span::call_site()))
        .collect();
    let val_vars: Vec<Ident> = chain
        .iter()
        .map(|m| Ident::new(&format!("__vtx_{}_val", m.name), Span::call_site()))
        .collect();

    let vtx_i = Ident::new("__vtx_i", Span::call_site());

    // Parse each node's core_expr_tokens once to extract arg, body, and extra deps.
    struct LinkData {
        arg: Ident,
        body: Expr,
        extra_dep_names: Vec<String>,
        extra_dep_lits: Vec<syn::LitStr>,
    }

    let links: Vec<LinkData> = chain
        .iter()
        .map(|meta| {
            let ts = meta.core_expr_tokens.clone();
            let node_input: NodeInput =
                syn::parse2(ts).expect("fusable node core_expr_tokens must parse");
            let NodeCall { closure, .. } =
                extract_node_call(&node_input.expr).expect("fusable node must be a row call");
            let args = closure_typed_args(closure);
            let arg = args[0].0.clone();
            let body = (*closure.body).clone();
            let mut dep_collector = BodyDepCollector {
                excluded: vec![arg.to_string(), meta.name.to_string()],
                deps: Vec::new(),
            };
            dep_collector.visit_expr(&body);
            let extra_dep_lits: Vec<syn::LitStr> = dep_collector
                .deps
                .iter()
                .map(|d| syn::LitStr::new(d, Span::call_site()))
                .collect();
            LinkData {
                arg,
                body,
                extra_dep_names: dep_collector.deps,
                extra_dep_lits,
            }
        })
        .collect();

    // Build the per-step code inside the fused loop.
    let loop_steps: Vec<proc_macro2::TokenStream> = links
        .iter()
        .enumerate()
        .map(|(i, link)| {
            let arg = &link.arg;
            let body = &link.body;
            let val_var = &val_vars[i];
            let data_var = &data_vars[i];

            // Primary input: first step reads from source; subsequent steps
            // read directly from the previous step's computed value.
            let input_expr = if i == 0 {
                quote! { #first_receiver.data[#vtx_i] }
            } else {
                let prev_val = &val_vars[i - 1];
                quote! { #prev_val }
            };

            // Extra body deps: earlier chain nodes → val var; external → .data[i].
            let extra_dep_binds: Vec<proc_macro2::TokenStream> = link
                .extra_dep_names
                .iter()
                .map(|dep_str| {
                    let dep_ident = Ident::new(dep_str, Span::call_site());
                    if let Some(&dep_idx) = idx_of.get(dep_str)
                        && dep_idx < i
                    {
                        let dep_val = &val_vars[dep_idx];
                        return quote! { let #dep_ident = #dep_val; };
                    }
                    quote! { let #dep_ident = #dep_ident.data[#vtx_i]; }
                })
                .collect();

            quote! {
                let #val_var = { let #arg = #input_expr; #(#extra_dep_binds)* #body };
                #data_var.push(#val_var);
            }
        })
        .collect();

    // Vec pre-allocations (all sized to the first receiver's length).
    let allocs = data_vars.iter().map(|dv| {
        quote! { let mut #dv: Vec<_> = Vec::with_capacity(#first_receiver.len()); }
    });

    // Post-loop Node bindings — primary receiver dep + any extra body deps.
    let bindings: Vec<proc_macro2::TokenStream> = chain
        .iter()
        .enumerate()
        .map(|(i, meta)| {
            let name = &meta.name;
            let name_lit = syn::LitStr::new(&meta.name.to_string(), Span::call_site());
            let data_var = &data_vars[i];
            let primary_dep = if i == 0 {
                syn::LitStr::new(&first_receiver.to_string(), Span::call_site())
            } else {
                syn::LitStr::new(&chain[i - 1].name.to_string(), Span::call_site())
            };
            let extra_dep_lits = &links[i].extra_dep_lits;
            quote! {
                let #name = vertexrs::Node::new_with_deps(
                    #name_lit, &[#primary_dep, #(#extra_dep_lits),*], #data_var,
                );
            }
        })
        .collect();

    quote! {
        #(#allocs)*
        for #vtx_i in 0..#first_receiver.len() {
            #(#loop_steps)*
        }
        #(#bindings)*
    }
}

/// Defines a computation pipeline: a sequence of typed source columns, derived
/// [`Node`](vertexrs::Node) transformations, and an explicit output schema.
///
/// # Syntax
/// ```text
/// pipeline! {
///     source!(name: T);                              // typed source column
///     node!(name = source.row(|x| expr));            // derived column
///     node!(name = source.col(|c| expr));            // column-wise operation
///     pipeline!(sub_name {                           // inline nested pipeline
///         [settings { failure: Hard | Soft | Isolate }]
///         source!(...); node!(...); output!(...)
///     });
///     sub!(expr => out_name: T, ...);                // external sub-pipeline
///     output!(col_a, col_b)                          // columns to expose
/// }
/// ```
///
/// Returns a [`Pipeline`](vertexrs::Pipeline) value.  Call `.push(&frame)` to
/// supply source data, then `.compute()` to run the kernels, then `.output()`
/// to read the result [`Frame`](vertexrs::Frame).
#[proc_macro]
pub fn pipeline(input: TokenStream) -> TokenStream {
    let PipelineDef { items } = parse_macro_input!(input as PipelineDef);

    let mut sources: Vec<PipelineSource> = Vec::new();
    let mut ordered: Vec<OrderedItem> = Vec::new();
    let mut output_names: Option<Vec<Ident>> = None;

    for item in items {
        match item {
            PipelineItem::Source(s) => sources.push(*s),
            PipelineItem::Ordered(o) => ordered.push(o),
            PipelineItem::Output(names) => {
                if output_names.is_some() {
                    return syn::Error::new(
                        Span::call_site(),
                        "pipeline! has more than one `output!(...)` declaration",
                    )
                    .to_compile_error()
                    .into();
                }
                output_names = Some(names);
            }
        }
    }

    let output_names = match output_names {
        Some(n) if !n.is_empty() => n,
        _ => {
            return syn::Error::new(
                Span::call_site(),
                "pipeline! requires an `output!(name, ...)` declaration with at least one column",
            )
            .to_compile_error()
            .into();
        }
    };

    // ── Pre-process ordered items, assigning sub indices ──────────────────────
    //
    // We collect struct-field, push, run, and init code for each ordered item
    // in a single pass so that the sub index counter is consistent everywhere.
    let mut ord_fields: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut ord_push: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut ord_run: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut ord_init: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut sub_idx: usize = 0;

    for group in group_fusable(&ordered) {
        match group {
            // ── Single node — existing per-node codegen path ──────────────────
            FusionGroup::Single(meta) => {
                ord_fields.push(quote! {});
                ord_push.push(quote! {});
                let ts = &meta.core_expr_tokens;
                let name = &meta.name;
                let name_str = name.to_string();

                let base_run = match meta.failure_override {
                    NodeFailureOverride::None => quote! {
                        vertexrs::node!(#ts);
                    },
                    NodeFailureOverride::Soft => {
                        let len_expr = if let Some(recv) = &meta.receiver_ident {
                            quote! { #recv.len() }
                        } else {
                            quote! { 0usize }
                        };
                        quote! {
                            let #name = match ::std::panic::catch_unwind(
                                ::std::panic::AssertUnwindSafe(|| {
                                    #[allow(unused_imports)]
                                    use vertexrs::{Node, BoolNode, ColRef};
                                    vertexrs::node!(#ts);
                                    #name
                                })
                            ) {
                                ::core::result::Result::Ok(__n) => __n,
                                ::core::result::Result::Err(__e) => {
                                    let __msg = __e.downcast_ref::<&str>()
                                        .map(|s| s.to_string())
                                        .or_else(|| __e.downcast_ref::<::std::string::String>().cloned())
                                        .unwrap_or_else(|| "kernel panicked".to_string());
                                    self.__warnings.push(::std::format!(
                                        "node '{}': kernel failed: {}", #name_str, __msg
                                    ));
                                    vertexrs::Node::all_na(#name_str, &[], #len_expr)
                                }
                            };
                        }
                    }
                    NodeFailureOverride::Hard => {
                        quote! {
                            let #name = ::std::panic::catch_unwind(
                                ::std::panic::AssertUnwindSafe(|| {
                                    #[allow(unused_imports)]
                                    use vertexrs::{Node, BoolNode, ColRef};
                                    vertexrs::node!(#ts);
                                    #name
                                })
                            ).map_err(|__e| {
                                let __msg = __e.downcast_ref::<&str>()
                                    .map(|s| s.to_string())
                                    .or_else(|| __e.downcast_ref::<::std::string::String>().cloned())
                                    .unwrap_or_else(|| "kernel panicked".to_string());
                                vertexrs::PipelineError::KernelPanic(
                                    ::std::format!("node '{}': {}", #name_str, __msg)
                                )
                            })?;
                        }
                    }
                };

                let run_code = if !meta.pure {
                    quote! {
                        #base_run
                        let #name = #name.with_pure(false);
                    }
                } else {
                    base_run
                };

                ord_run.push(run_code);
                ord_init.push(quote! {});
            }

            // ── Fused chain — one loop for all nodes in the chain ─────────────
            FusionGroup::Fused(chain) => {
                ord_fields.push(quote! {});
                ord_push.push(quote! {});
                ord_run.push(emit_fused_chain(&chain));
                ord_init.push(quote! {});
            }

            // ── pipeline!(name { ... }) — nested inline pipeline ──────────────
            FusionGroup::Nested(n) => {
                let name = &n.name;
                let inner = &n.inner_tokens;
                let name_str = n.name.to_string();

                let run_code = match n.failure_mode {
                    FailureModeKind::Hard => quote! {
                        self.#name.compute()?;
                        let #name = self.#name.output().clone();
                    },
                    FailureModeKind::Soft => quote! {
                        let #name = {
                            let __res = self.#name.compute();
                            match __res {
                                ::core::result::Result::Ok(()) => self.#name.output().clone(),
                                ::core::result::Result::Err(__e) => {
                                    self.__warnings.push(
                                        ::std::format!("pipeline '{}' failed: {}", #name_str, __e)
                                    );
                                    vertexrs::Frame::new()
                                }
                            }
                        };
                    },
                    FailureModeKind::Isolate => quote! {
                        let #name = {
                            let __res = self.#name.compute();
                            match __res {
                                ::core::result::Result::Ok(()) => self.#name.output().clone(),
                                ::core::result::Result::Err(__e) => {
                                    self.__isolated_errors.push(__e);
                                    vertexrs::Frame::new()
                                }
                            }
                        };
                    },
                };

                ord_fields.push(quote! { #name: vertexrs::Pipeline, });
                ord_push.push(quote! { self.#name.push(frame); });
                ord_run.push(run_code);
                ord_init.push(quote! { #name: vertexrs::pipeline!(#inner), });
            }

            // ── sub!(expr => out: T, ...) — external sub-pipeline ─────────────
            FusionGroup::Sub(s) => {
                let field_name = Ident::new(&format!("__sub_{sub_idx}"), Span::call_site());
                sub_idx += 1;
                let expr = &s.expr;

                let output_extracts = s.outputs.iter().map(|(out_name, out_ty)| {
                    let lit = syn::LitStr::new(&out_name.to_string(), out_name.span());
                    quote! {
                        let #out_name = {
                            let __buf = self.#field_name
                                .output()
                                .get::<#out_ty>(#lit)
                                .ok_or(vertexrs::PipelineError::MissingSource(#lit))?;
                            vertexrs::Node::<#out_ty>::from_data(#lit, __buf.to_vec())
                        };
                    }
                });

                ord_fields.push(quote! { #field_name: vertexrs::Pipeline, });
                ord_push.push(quote! { self.#field_name.push(frame); });
                ord_run.push(quote! {
                    self.#field_name.compute()?;
                    #(#output_extracts)*
                });
                ord_init.push(quote! { #field_name: #expr, });
            }
        }
    }

    // ── Source fields, push, run, init ────────────────────────────────────────
    let source_fields = sources.iter().map(|s| {
        let name = &s.name;
        let ty = &s.ty;
        quote! { #name: ::core::option::Option<vertexrs::Node<#ty>>, }
    });

    let source_push_impls = sources.iter().map(|s| {
        let name = &s.name;
        let ty = &s.ty;
        let name_lit = syn::LitStr::new(&name.to_string(), name.span());
        quote! {
            if let Some(__vtx_data) = frame.get::<#ty>(#name_lit) {
                self.#name = Some(vertexrs::Node::from_data(#name_lit, __vtx_data.to_vec()));
            }
        }
    });

    let source_binds = sources.iter().map(|s| {
        let name = &s.name;
        let name_lit = syn::LitStr::new(&name.to_string(), name.span());
        quote! {
            let #name = self.#name.clone()
                .ok_or(vertexrs::PipelineError::MissingSource(#name_lit))?;
        }
    });

    let output_pushes = output_names.iter().map(|name| {
        quote! { __vtx_output.push_node(vertexrs::AnyNode::from(#name)); }
    });

    let source_inits = sources.iter().map(|s| {
        let name = &s.name;
        quote! { #name: ::core::option::Option::None, }
    });

    quote! {
        {
            struct __VtxPipeline {
                #(#source_fields)*
                #(#ord_fields)*
                __output: vertexrs::Frame,
                __warnings: ::std::vec::Vec<::std::string::String>,
                __isolated_errors: ::std::vec::Vec<vertexrs::PipelineError>,
            }

            impl vertexrs::pipeline::PipelineImpl for __VtxPipeline {
                fn push_sources(&mut self, frame: &vertexrs::Frame) {
                    #(#source_push_impls)*
                    #(#ord_push)*
                }

                fn run(
                    &mut self,
                ) -> ::core::result::Result<(), vertexrs::PipelineError> {
                    #![allow(unused_variables)]
                    #[allow(unused_imports)]
                    use vertexrs::{Node, ColRef};
                    #(#source_binds)*
                    #(#ord_run)*
                    let mut __vtx_output = vertexrs::Frame::new();
                    #(#output_pushes)*
                    self.__output = __vtx_output;
                    ::core::result::Result::Ok(())
                }

                fn output(&self) -> &vertexrs::Frame {
                    &self.__output
                }

                fn drain_warnings(
                    &mut self,
                ) -> ::std::vec::Vec<::std::string::String> {
                    ::std::mem::take(&mut self.__warnings)
                }

                fn drain_isolated_errors(
                    &mut self,
                ) -> ::std::vec::Vec<vertexrs::PipelineError> {
                    ::std::mem::take(&mut self.__isolated_errors)
                }
            }

            vertexrs::Pipeline::new(::std::boxed::Box::new(__VtxPipeline {
                #(#source_inits)*
                #(#ord_init)*
                __output: vertexrs::Frame::new(),
                __warnings: ::std::vec::Vec::new(),
                __isolated_errors: ::std::vec::Vec::new(),
            }))
        }
    }
    .into()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_str;

    // Helper: parse a closure literal from a string.
    fn parse_closure(src: &str) -> ExprClosure {
        parse_str::<ExprClosure>(src).expect("failed to parse closure")
    }

    // ── closure_typed_args ────────────────────────────────────────────────────

    #[test]
    fn typed_args_untyped_single() {
        let c = parse_closure("|x| x + 1");
        let args = closure_typed_args(&c);
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].0.to_string(), "x");
        assert!(args[0].1.is_none());
    }

    #[test]
    fn typed_args_typed_single() {
        let c = parse_closure("|price: f64| price * 2.0");
        let args = closure_typed_args(&c);
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].0.to_string(), "price");
        assert!(args[0].1.is_some());
    }

    #[test]
    fn typed_args_multiple_typed() {
        let c = parse_closure("|price: f64, qty: i64| price * qty as f64");
        let args = closure_typed_args(&c);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].0.to_string(), "price");
        assert!(args[0].1.is_some());
        assert_eq!(args[1].0.to_string(), "qty");
        assert!(args[1].1.is_some());
    }

    #[test]
    fn typed_args_mixed_not_returned_for_untyped() {
        // An untyped arg alongside typed ones should still parse.
        let c = parse_closure("|x, price: f64| x + price");
        let args = closure_typed_args(&c);
        assert_eq!(args.len(), 2);
        assert!(args[0].1.is_none()); // x — untyped
        assert!(args[1].1.is_some()); // price: f64 — typed
    }

    // ── BodyDepCollector ──────────────────────────────────────────────────────

    #[test]
    fn dep_collector_finds_bare_idents() {
        let body: Expr = parse_str("x + tax").unwrap();
        let mut collector = BodyDepCollector {
            excluded: vec!["x".to_owned()],
            deps: Vec::new(),
        };
        collector.visit_expr(&body);
        assert_eq!(collector.deps, vec!["tax"]);
    }

    #[test]
    fn dep_collector_excludes_multiple() {
        let body: Expr = parse_str("price * qty as f64 + extra").unwrap();
        let mut collector = BodyDepCollector {
            excluded: vec!["price".to_owned(), "qty".to_owned()],
            deps: Vec::new(),
        };
        collector.visit_expr(&body);
        assert_eq!(collector.deps, vec!["extra"]);
    }

    #[test]
    fn dep_collector_ignores_multi_segment_paths() {
        let body: Expr = parse_str("std::f64::MAX + dep").unwrap();
        let mut collector = BodyDepCollector {
            excluded: vec![],
            deps: Vec::new(),
        };
        collector.visit_expr(&body);
        // std::f64::MAX is multi-segment; only `dep` is a bare ident.
        assert_eq!(collector.deps, vec!["dep"]);
    }

    // ── Fusion helper tests ───────────────────────────────────────────────────

    // Build a `NodeItemMeta` from plain strings — mirrors `parse_node_item_meta`
    // logic without going through the full proc-macro parse stream.
    fn make_node_meta(
        name: &str,
        core_expr: &str,
        failure_override: NodeFailureOverride,
        pure: bool,
    ) -> NodeItemMeta {
        let name_ident: Ident = parse_str(name).unwrap();
        let core_expr_ts: proc_macro2::TokenStream =
            format!("{name} = {core_expr}").parse().unwrap();
        let expr: Expr = parse_str(core_expr).unwrap();
        let recv_ident =
            extract_node_call(&expr).and_then(|nc| receiver_ident(nc.receiver).cloned());
        NodeItemMeta {
            name: name_ident,
            receiver_ident: recv_ident,
            core_expr_tokens: core_expr_ts,
            failure_override,
            pure,
        }
    }

    fn node_ordered(meta: NodeItemMeta) -> OrderedItem {
        OrderedItem::Node(Box::new(meta))
    }

    // ── is_row_fusable ────────────────────────────────────────────────────────

    #[test]
    fn fusable_pure_row_single_untyped_arg() {
        let meta = make_node_meta("b", "a.row(|x| x * 2.0)", NodeFailureOverride::None, true);
        assert!(is_row_fusable(&meta));
    }

    #[test]
    fn not_fusable_impure_node() {
        let meta = make_node_meta("b", "a.row(|x| x * 2.0)", NodeFailureOverride::None, false);
        assert!(!is_row_fusable(&meta));
    }

    #[test]
    fn not_fusable_soft_failure_override() {
        let meta = make_node_meta("b", "a.row(|x| x * 2.0)", NodeFailureOverride::Soft, true);
        assert!(!is_row_fusable(&meta));
    }

    #[test]
    fn not_fusable_hard_failure_override() {
        let meta = make_node_meta("b", "a.row(|x| x * 2.0)", NodeFailureOverride::Hard, true);
        assert!(!is_row_fusable(&meta));
    }

    #[test]
    fn not_fusable_col_mode() {
        let meta = make_node_meta("b", "a.col(|c| c)", NodeFailureOverride::None, true);
        assert!(!is_row_fusable(&meta));
    }

    #[test]
    fn not_fusable_typed_arg() {
        let meta = make_node_meta(
            "b",
            "a.row(|x: f64| x * 2.0)",
            NodeFailureOverride::None,
            true,
        );
        assert!(!is_row_fusable(&meta));
    }

    #[test]
    fn not_fusable_explicit_bool_return() {
        let meta = make_node_meta(
            "b",
            "a.row(|x| -> bool { x > 0.0 })",
            NodeFailureOverride::None,
            true,
        );
        assert!(!is_row_fusable(&meta));
    }

    // ── chains_from ───────────────────────────────────────────────────────────

    #[test]
    fn chains_from_linked_pair() {
        let a = make_node_meta("a", "price.row(|x| x)", NodeFailureOverride::None, true);
        let b = make_node_meta("b", "a.row(|x| x * 2.0)", NodeFailureOverride::None, true);
        assert!(chains_from(&a, &b));
    }

    #[test]
    fn chains_from_different_receiver() {
        let a = make_node_meta("a", "price.row(|x| x)", NodeFailureOverride::None, true);
        let c = make_node_meta(
            "c",
            "price.row(|x| x + 1.0)",
            NodeFailureOverride::None,
            true,
        );
        assert!(!chains_from(&a, &c));
    }

    #[test]
    fn chains_from_no_receiver() {
        // A node whose receiver is not a simple ident gets receiver_ident = None.
        let a = make_node_meta("a", "price.row(|x| x)", NodeFailureOverride::None, true);
        // Manually create a meta with no receiver_ident.
        let no_recv = NodeItemMeta {
            name: parse_str("z").unwrap(),
            receiver_ident: None,
            core_expr_tokens: "z = price.row(|x| x)".parse().unwrap(),
            failure_override: NodeFailureOverride::None,
            pure: true,
        };
        assert!(!chains_from(&a, &no_recv));
    }

    // ── group_fusable ─────────────────────────────────────────────────────────

    #[test]
    fn group_fusable_single_fusable_node_stays_single() {
        let items = vec![node_ordered(make_node_meta(
            "a",
            "price.row(|x| x * 2.0)",
            NodeFailureOverride::None,
            true,
        ))];
        let groups = group_fusable(&items);
        assert_eq!(groups.len(), 1);
        assert!(matches!(groups[0], FusionGroup::Single(_)));
    }

    #[test]
    fn group_fusable_impure_node_emits_single() {
        let items = vec![node_ordered(make_node_meta(
            "a",
            "price.row(|x| x * 2.0)",
            NodeFailureOverride::None,
            false,
        ))];
        let groups = group_fusable(&items);
        assert_eq!(groups.len(), 1);
        assert!(matches!(groups[0], FusionGroup::Single(_)));
    }

    #[test]
    fn group_fusable_two_node_linear_chain_fuses() {
        let items = vec![
            node_ordered(make_node_meta(
                "a",
                "price.row(|x| x * 2.0)",
                NodeFailureOverride::None,
                true,
            )),
            node_ordered(make_node_meta(
                "b",
                "a.row(|x| x + 1.0)",
                NodeFailureOverride::None,
                true,
            )),
        ];
        let groups = group_fusable(&items);
        assert_eq!(groups.len(), 1);
        assert!(matches!(&groups[0], FusionGroup::Fused(chain) if chain.len() == 2));
    }

    #[test]
    fn group_fusable_fan_out_prevents_fusion() {
        // `a` has two consumers (b and c), so neither link fuses.
        let items = vec![
            node_ordered(make_node_meta(
                "a",
                "price.row(|x| x * 2.0)",
                NodeFailureOverride::None,
                true,
            )),
            node_ordered(make_node_meta(
                "b",
                "a.row(|x| x + 10.0)",
                NodeFailureOverride::None,
                true,
            )),
            node_ordered(make_node_meta(
                "c",
                "a.row(|x| x - 1.0)",
                NodeFailureOverride::None,
                true,
            )),
        ];
        let groups = group_fusable(&items);
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().all(|g| matches!(g, FusionGroup::Single(_))));
    }

    #[test]
    fn group_fusable_soft_node_splits_chain() {
        // [a(pure), b(pure)] → soft(c) → [d(pure), e(pure)]
        // Produces: Fused([a,b]), Single(c), Fused([d,e]).
        let items = vec![
            node_ordered(make_node_meta(
                "a",
                "price.row(|x| x * 2.0)",
                NodeFailureOverride::None,
                true,
            )),
            node_ordered(make_node_meta(
                "b",
                "a.row(|x| x + 1.0)",
                NodeFailureOverride::None,
                true,
            )),
            node_ordered(make_node_meta(
                "c",
                "b.row(|x| x - 0.5)",
                NodeFailureOverride::Soft,
                true,
            )),
            node_ordered(make_node_meta(
                "d",
                "c.row(|x| x * x)",
                NodeFailureOverride::None,
                true,
            )),
            node_ordered(make_node_meta(
                "e",
                "d.row(|x| x / 2.0)",
                NodeFailureOverride::None,
                true,
            )),
        ];
        let groups = group_fusable(&items);
        assert_eq!(groups.len(), 3);
        assert!(matches!(&groups[0], FusionGroup::Fused(chain) if chain.len() == 2));
        assert!(matches!(&groups[1], FusionGroup::Single(_)));
        assert!(matches!(&groups[2], FusionGroup::Fused(chain) if chain.len() == 2));
    }

    #[test]
    fn group_fusable_five_node_linear_chain() {
        let items = vec![
            node_ordered(make_node_meta(
                "a",
                "price.row(|x| x * 2.0)",
                NodeFailureOverride::None,
                true,
            )),
            node_ordered(make_node_meta(
                "b",
                "a.row(|x| x + 1.0)",
                NodeFailureOverride::None,
                true,
            )),
            node_ordered(make_node_meta(
                "c",
                "b.row(|x| x - 0.5)",
                NodeFailureOverride::None,
                true,
            )),
            node_ordered(make_node_meta(
                "d",
                "c.row(|x| x * x)",
                NodeFailureOverride::None,
                true,
            )),
            node_ordered(make_node_meta(
                "e",
                "d.row(|x| x / 2.0)",
                NodeFailureOverride::None,
                true,
            )),
        ];
        let groups = group_fusable(&items);
        assert_eq!(groups.len(), 1);
        assert!(matches!(&groups[0], FusionGroup::Fused(chain) if chain.len() == 5));
    }

    #[test]
    fn group_fusable_empty_input() {
        let groups = group_fusable(&[]);
        assert!(groups.is_empty());
    }

    // ── emit_fused_chain ──────────────────────────────────────────────────────

    #[test]
    fn emit_fused_chain_two_node_produces_tokens() {
        let a = make_node_meta(
            "a",
            "price.row(|x| x * 2.0)",
            NodeFailureOverride::None,
            true,
        );
        let b = make_node_meta("b", "a.row(|x| x + 1.0)", NodeFailureOverride::None, true);
        let chain: Vec<&NodeItemMeta> = vec![&a, &b];
        let ts = emit_fused_chain(&chain);
        assert!(!ts.is_empty());
        let s = ts.to_string();
        // Fused loop should pre-allocate accumulators and emit a for-loop.
        assert!(s.contains("__vtx_a_data"), "missing a accumulator in: {s}");
        assert!(s.contains("__vtx_b_data"), "missing b accumulator in: {s}");
        assert!(s.contains("__vtx_i"), "missing loop var in: {s}");
    }

    #[test]
    fn emit_fused_chain_five_nodes_binds_all_names() {
        let nodes: Vec<NodeItemMeta> = [
            ("a", "price.row(|x| x * 2.0)"),
            ("b", "a.row(|x| x + 1.0)"),
            ("c", "b.row(|x| x - 0.5)"),
            ("d", "c.row(|x| x * x)"),
            ("e", "d.row(|x| x / 2.0)"),
        ]
        .into_iter()
        .map(|(n, e)| make_node_meta(n, e, NodeFailureOverride::None, true))
        .collect();
        let refs: Vec<&NodeItemMeta> = nodes.iter().collect();
        let ts = emit_fused_chain(&refs);
        let s = ts.to_string();
        for name in ["a", "b", "c", "d", "e"] {
            assert!(
                s.contains(&format!("__vtx_{name}_data")),
                "missing accumulator for {name}"
            );
        }
        // Node::new_with_deps bindings must appear for each node.
        assert!(s.contains("new_with_deps"), "missing new_with_deps in: {s}");
    }
}
