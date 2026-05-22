use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    visit::Visit,
    Expr, ExprClosure, ExprPath, Ident, Pat, ReturnType, Token, Type,
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
    let Expr::MethodCall(call) = expr else { return None };
    let mode = match call.method.to_string().as_str() {
        "row" => AccessMode::Row,
        "col" => AccessMode::Col,
        _ => return None,
    };
    if call.args.len() != 1 {
        return None;
    }
    let Expr::Closure(closure) = &call.args[0] else { return None };
    Some(NodeCall { receiver: &call.receiver, mode, closure })
}

/// Extracts the single identifier from a receiver expression, e.g. `price` from
/// `price`.  Returns `None` for complex receivers.
fn receiver_ident(expr: &Expr) -> Option<&Ident> {
    let Expr::Path(ExprPath { qself: None, path, .. }) = expr else { return None };
    if path.segments.len() == 1 { Some(&path.segments[0].ident) } else { None }
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
/// ```ignore
/// node!(tax   = price.row(|x| x * 0.2));
/// node!(total = price.row(|x| x + tax));   // `tax` is another node
/// ```
///
/// **Col mode** — whole-column operation (may change length):
/// ```ignore
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

    let Some(NodeCall { receiver, mode, closure }) = extract_node_call(&expr) else {
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

                if is_bool_return(closure) {
                    quote! {
                        let #name = BoolNode::new_with_deps(
                            #name_lit,
                            &[#recv_lit, #(#extra_dep_lits),*],
                            (0..#recv_ident_ts.len())
                                .map(|__vtx_i| {
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
                                .map(|__vtx_i| {
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
    Node(proc_macro2::TokenStream),
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
                return Err(syn::Error::new(key.span(), "expected `failure` key in settings block"));
            }
            settings_inner.parse::<Token![:]>()?;
            let mode: Ident = settings_inner.parse()?;
            return match mode.to_string().as_str() {
                "Soft"    => Ok(FailureModeKind::Soft),
                "Hard"    => Ok(FailureModeKind::Hard),
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
                        if content.is_empty() { break; }
                        content.parse::<Token![,]>()?;
                        if content.is_empty() { break; } // trailing comma
                    }
                }
                "node" => {
                    let tokens: proc_macro2::TokenStream = content.parse()?;
                    items.push(PipelineItem::Ordered(OrderedItem::Node(tokens)));
                }
                "output" => {
                    let mut names = Vec::new();
                    while !content.is_empty() {
                        names.push(content.parse::<Ident>()?);
                        if content.is_empty() { break; }
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
                        NestedPipelineItem { name, failure_mode, inner_tokens },
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
                        if content.is_empty() { break; }
                        content.parse::<Token![,]>()?;
                        if content.is_empty() { break; } // trailing comma
                    }
                    items.push(PipelineItem::Ordered(OrderedItem::Sub(Box::new(
                        SubItem { expr, outputs },
                    ))));
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

    for item in &ordered {
        match item {
            // ── node! — no field, no push; just a run-time invocation ─────────
            OrderedItem::Node(ts) => {
                ord_fields.push(quote! {});
                ord_push.push(quote! {});
                ord_run.push(quote! { vertexrs::node!(#ts); });
                ord_init.push(quote! {});
            }

            // ── pipeline!(name { ... }) — nested inline pipeline ──────────────
            OrderedItem::Nested(n) => {
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
            OrderedItem::Sub(s) => {
                let field_name =
                    Ident::new(&format!("__sub_{sub_idx}"), Span::call_site());
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
}
