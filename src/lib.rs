//! Trade-Lang 符号定义宏
//!
//! 提供 `define_symbol!` 过程宏，用于同时生成：
//!   1. `SymbolMetadata`（供 Checker 语义检查）
//!   2. 类型化 Handler trait（供解释器实现，带 Rust 编译期类型检查）
//!   3. Adapter 结构体（桥接类型化 trait → 通用 Handler trait）
//!
//! # 语法
//!
//! ```ignore
//! define_symbol! {
//!     pub executor PumpBuy {
//!         context need PumpTradeCtx => "pump_trade";
//!         param amount: Amount;
//!         param slippage: Percent;
//!         returns (Price, Price);
//!     }
//! }
//! ```
//!
//! 支持的类别：`monitor`、`executor`、`data_item`、`condition`
//!
//! 上下文操作：`produce`（产出）、`need`（只读使用）、`consume`（消费移除）
//!
//! DSL 类型：`Price`、`Amount`、`Duration`、`TimePoint`、`Percent`、`Count`、
//!           `Number`、`String`、`Bool`、`Address`、`Any`

extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitStr, Token, Type, Visibility, braced, bracketed, parenthesized};

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// DSL Type Mapping
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Clone, Debug)]
enum TradeType {
    Price,
    Amount,
    Duration,
    TimePoint,
    Percent,
    Count,
    Number,
    StringTy,
    Bool,
    Address,
    Any,
    /// 列表类型，如 [Address]
    List(Box<TradeType>),
    /// 元组类型，如 (Price, Amount)
    Tuple(Vec<TradeType>),
    /// 联合类型，如 Percent | Amount
    Union(Vec<TradeType>),
    /// 命名别名，通过 TypeAlias inventory 运行时解析
    Alias(String),
}

impl TradeType {
    fn from_ident(id: &Ident) -> syn::Result<Self> {
        match id.to_string().as_str() {
            "Price" => Ok(Self::Price),
            "Amount" => Ok(Self::Amount),
            "Duration" => Ok(Self::Duration),
            "TimePoint" => Ok(Self::TimePoint),
            "Percent" => Ok(Self::Percent),
            "Count" => Ok(Self::Count),
            "Number" => Ok(Self::Number),
            "String" => Ok(Self::StringTy),
            "Bool" => Ok(Self::Bool),
            "Address" => Ok(Self::Address),
            "Any" => Ok(Self::Any),
            other => Ok(Self::Alias(other.to_string())),
        }
    }

    /// DSL 类型 → Rust 类型 token
    fn rust_type(&self) -> TokenStream2 {
        match self {
            Self::Price
            | Self::Amount
            | Self::Percent
            | Self::Count
            | Self::Number
            | Self::TimePoint => quote!(f64),
            Self::Duration => quote!(u64),
            Self::StringTy | Self::Address => quote!(::std::string::String),
            Self::Bool => quote!(bool),
            Self::Any => quote!(trade_meta_compiler::RuntimeValue),
            Self::List(elem) => {
                let inner = elem.rust_type();
                quote!(::std::vec::Vec<#inner>)
            }
            Self::Tuple(elems) => {
                let types: Vec<_> = elems.iter().map(|e| e.rust_type()).collect();
                quote!((#(#types),*))
            }
            Self::Union(_) | Self::Alias(_) => quote!(trade_meta_compiler::RuntimeValue),
        }
    }

    /// DSL 类型 → TypeSpec 构造 token
    fn type_spec(&self) -> TokenStream2 {
        match self {
            Self::Price => quote!(trade_meta_compiler::TypeSpec::Price),
            Self::Amount => quote!(trade_meta_compiler::TypeSpec::Amount),
            Self::Duration => quote!(trade_meta_compiler::TypeSpec::Duration),
            Self::TimePoint => quote!(trade_meta_compiler::TypeSpec::TimePoint),
            Self::Percent => quote!(trade_meta_compiler::TypeSpec::Percent),
            Self::Count => quote!(trade_meta_compiler::TypeSpec::Count),
            Self::Number => quote!(trade_meta_compiler::TypeSpec::Number),
            Self::StringTy => quote!(trade_meta_compiler::TypeSpec::String),
            Self::Bool => quote!(trade_meta_compiler::TypeSpec::Bool),
            Self::Address => quote!(trade_meta_compiler::TypeSpec::Address),
            Self::Any => quote!(trade_meta_compiler::TypeSpec::Any),
            Self::List(elem) => {
                let inner = elem.type_spec();
                quote!(trade_meta_compiler::TypeSpec::List(Box::new(#inner)))
            }
            Self::Tuple(elems) => {
                let specs: Vec<_> = elems.iter().map(|e| e.type_spec()).collect();
                quote!(trade_meta_compiler::TypeSpec::Tuple(vec![#(#specs),*]))
            }
            // Union/Alias 不应通过 type_spec() 单独使用，metadata 生成时特殊处理
            Self::Union(_) | Self::Alias(_) => quote!(trade_meta_compiler::TypeSpec::Any),
        }
    }

    /// 从 RuntimeValue 提取该类型的值（用于 adapter 参数提取）
    fn extract_from_rv(&self, rv_expr: &TokenStream2) -> TokenStream2 {
        match self {
            Self::Price
            | Self::Amount
            | Self::Percent
            | Self::Count
            | Self::Number
            | Self::TimePoint => quote!(#rv_expr.as_f64()),
            Self::Duration => quote!(#rv_expr.as_f64() as u64),
            Self::StringTy | Self::Address => quote!({
                match #rv_expr {
                    trade_meta_compiler::RuntimeValue::Str(s) => s.clone(),
                    _ => ::std::string::String::new(),
                }
            }),
            Self::Bool => quote!({
                match #rv_expr {
                    trade_meta_compiler::RuntimeValue::Bool(b) => *b,
                    _ => false,
                }
            }),
            Self::Any => quote!(#rv_expr.clone()),
            Self::List(elem) => {
                let elem_extract = elem.extract_from_rv(&quote!(__item));
                quote!({
                    match #rv_expr {
                        trade_meta_compiler::RuntimeValue::List(items) => {
                            items.iter().map(|__item| #elem_extract).collect()
                        }
                        _ => ::std::vec::Vec::new(),
                    }
                })
            }
            Self::Tuple(elems) => {
                let n = elems.len();
                let extracts: Vec<_> = elems
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let item_expr = quote!(__tup_items[#i]);
                        let extract = e.extract_from_rv(&item_expr);
                        quote!(#extract)
                    })
                    .collect();
                quote!({
                    match #rv_expr {
                        trade_meta_compiler::RuntimeValue::Tuple(__tup_items) if __tup_items.len() >= #n => {
                            (#(#extracts),*)
                        }
                        _ => Default::default(),
                    }
                })
            }
            Self::Union(_) | Self::Alias(_) => quote!(#rv_expr.clone()),
        }
    }

    /// 将 Rust 值包装为 RuntimeValue（用于 adapter 返回值打包）
    fn wrap_in_rv(&self, expr: &TokenStream2) -> TokenStream2 {
        match self {
            Self::Price => quote!(trade_meta_compiler::RuntimeValue::Price(#expr)),
            Self::Amount => {
                quote!(trade_meta_compiler::RuntimeValue::Amount(#expr, ::std::string::String::new()))
            }
            Self::Duration => {
                quote!(trade_meta_compiler::RuntimeValue::Duration(#expr as f64))
            }
            Self::TimePoint => quote!(trade_meta_compiler::RuntimeValue::TimePoint(#expr)),
            Self::Percent => quote!(trade_meta_compiler::RuntimeValue::Percent(#expr)),
            Self::Count => quote!(trade_meta_compiler::RuntimeValue::Count(#expr)),
            Self::Number => quote!(trade_meta_compiler::RuntimeValue::Number(#expr)),
            Self::StringTy | Self::Address => {
                quote!(trade_meta_compiler::RuntimeValue::Str(#expr))
            }
            Self::Bool => quote!(trade_meta_compiler::RuntimeValue::Bool(#expr)),
            Self::Any => quote!(#expr),
            Self::List(elem) => {
                let elem_wrap = elem.wrap_in_rv(&quote!(__item));
                quote!(trade_meta_compiler::RuntimeValue::List(
                    #expr.into_iter().map(|__item| #elem_wrap).collect()
                ))
            }
            Self::Tuple(elems) => {
                let wraps: Vec<_> = elems
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let idx = syn::Index::from(i);
                        let field = quote!(#expr.#idx);
                        e.wrap_in_rv(&field)
                    })
                    .collect();
                quote!(trade_meta_compiler::RuntimeValue::Tuple(vec![#(#wraps),*]))
            }
            Self::Union(_) | Self::Alias(_) => quote!(#expr),
        }
    }

    /// 是否为联合类型或别名（handler 签名使用 RuntimeValue）
    fn is_runtime_value_type(&self) -> bool {
        matches!(self, Self::Union(_) | Self::Alias(_) | Self::Any)
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Context / Category enums
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Clone, Debug)]
enum CtxOp {
    Produce,
    Need,
    Consume,
}

#[derive(Clone, Debug, PartialEq)]
enum Category {
    Monitor,
    Executor,
    DataItem,
    Condition,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// AST types
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

struct SymbolDef {
    vis: Visibility,
    category: Category,
    name: Ident,
    contexts: Vec<ContextDef>,
    params: Vec<ParamDef>,
    returns: Option<ReturnsDef>,
}

struct ContextDef {
    op: CtxOp,
    ty: Type,
    protocol: LitStr,
}

struct ParamDef {
    name: Ident,
    trade_type: TradeType,
    required: bool,
}

struct ReturnsDef {
    types: Vec<TradeType>,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Parsing
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// 解析单个类型：`Type`、`[Type]`（列表）或 `(Type, Type)`（元组）
fn parse_single_trade_type(input: ParseStream) -> syn::Result<TradeType> {
    if input.peek(syn::token::Bracket) {
        let inner;
        bracketed!(inner in input);
        let type_ident: Ident = inner.parse()?;
        let elem = TradeType::from_ident(&type_ident)?;
        Ok(TradeType::List(Box::new(elem)))
    } else if input.peek(syn::token::Paren) {
        let inner;
        parenthesized!(inner in input);
        let mut types = Vec::new();
        loop {
            if inner.is_empty() {
                break;
            }
            types.push(parse_trade_type(&inner)?);
            if inner.peek(Token![,]) {
                inner.parse::<Token![,]>()?;
            } else {
                break;
            }
        }
        Ok(TradeType::Tuple(types))
    } else {
        let type_ident: Ident = input.parse()?;
        TradeType::from_ident(&type_ident)
    }
}

/// 解析类型，支持联合类型 `Percent | Amount`
fn parse_trade_type(input: ParseStream) -> syn::Result<TradeType> {
    let first = parse_single_trade_type(input)?;

    if input.peek(Token![|]) {
        let mut types = vec![first];
        while input.peek(Token![|]) {
            input.parse::<Token![|]>()?;
            types.push(parse_single_trade_type(input)?);
        }
        Ok(TradeType::Union(types))
    } else {
        Ok(first)
    }
}

impl Parse for SymbolDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let vis: Visibility = input.parse()?;

        // category keyword
        let cat_ident: Ident = input.parse()?;
        let category = match cat_ident.to_string().as_str() {
            "monitor" => Category::Monitor,
            "executor" => Category::Executor,
            "data_item" => Category::DataItem,
            "condition" => Category::Condition,
            other => {
                return Err(syn::Error::new(
                    cat_ident.span(),
                    format!(
                        "Expected 'monitor', 'executor', 'data_item', or 'condition', got '{}'",
                        other
                    ),
                ));
            }
        };

        let name: Ident = input.parse()?;

        let content;
        braced!(content in input);

        let mut contexts = Vec::new();
        let mut params = Vec::new();
        let mut returns = None;

        while !content.is_empty() {
            // peek: 如果是 `ref` 关键字开头的 context 行？不，context 行以 "context" ident 开头
            let kw: Ident = content.parse()?;
            match kw.to_string().as_str() {
                "context" => {
                    // context need Type => "protocol";
                    // context produce Type => "protocol";
                    let op_ident: Ident = content.parse()?;
                    let op = match op_ident.to_string().as_str() {
                        "produce" => CtxOp::Produce,
                        "need" => CtxOp::Need,
                        "consume" => CtxOp::Consume,
                        other => {
                            return Err(syn::Error::new(
                                op_ident.span(),
                                format!(
                                    "Expected 'produce', 'need', or 'consume', got '{}'",
                                    other
                                ),
                            ));
                        }
                    };
                    let ty: Type = content.parse()?;
                    content.parse::<Token![=>]>()?;
                    let protocol: LitStr = content.parse()?;
                    content.parse::<Token![;]>()?;
                    contexts.push(ContextDef { op, ty, protocol });
                }
                "param" => {
                    // param name: Type;  or  param name: [Type];
                    let pname: Ident = content.parse()?;
                    content.parse::<Token![:]>()?;
                    let trade_type = parse_trade_type(&content)?;
                    content.parse::<Token![;]>()?;
                    params.push(ParamDef {
                        name: pname,
                        trade_type,
                        required: true,
                    });
                }
                "optional" => {
                    // optional param name: Type;  or  optional param name: [Type];
                    let _param_kw: Ident = content.parse()?; // consume "param"
                    let pname: Ident = content.parse()?;
                    content.parse::<Token![:]>()?;
                    let trade_type = parse_trade_type(&content)?;
                    content.parse::<Token![;]>()?;
                    params.push(ParamDef {
                        name: pname,
                        trade_type,
                        required: false,
                    });
                }
                "returns" => {
                    // returns (Type, Type); or returns Type;
                    if content.peek(syn::token::Paren) {
                        let inner;
                        parenthesized!(inner in content);
                        let mut types = Vec::new();
                        loop {
                            if inner.is_empty() {
                                break;
                            }
                            let type_ident: Ident = inner.parse()?;
                            types.push(TradeType::from_ident(&type_ident)?);
                            if inner.peek(Token![,]) {
                                inner.parse::<Token![,]>()?;
                            } else {
                                break;
                            }
                        }
                        returns = Some(ReturnsDef { types });
                    } else {
                        let type_ident: Ident = content.parse()?;
                        returns = Some(ReturnsDef {
                            types: vec![TradeType::from_ident(&type_ident)?],
                        });
                    }
                    content.parse::<Token![;]>()?;
                }
                other => {
                    return Err(syn::Error::new(
                        kw.span(),
                        format!(
                            "Expected 'context', 'param', 'optional', or 'returns', got '{}'",
                            other
                        ),
                    ));
                }
            }
        }

        Ok(SymbolDef {
            vis,
            category,
            name,
            contexts,
            params,
            returns,
        })
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Helpers
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = s.as_bytes()[i - 1] as char;
            if prev.is_lowercase() || prev.is_ascii_digit() {
                result.push('_');
            }
        }
        result.push(c.to_ascii_lowercase());
    }
    result
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Code Generation
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

fn generate(def: &SymbolDef) -> TokenStream2 {
    let metadata_fn = gen_metadata(def);
    let handler_trait = gen_handler_trait(def);
    let adapter = gen_adapter(def);
    let fn_name = format_ident!("{}_metadata", to_snake_case(&def.name.to_string()));

    quote! {
        #metadata_fn
        #handler_trait
        #adapter

        trade_meta_compiler::inventory::submit! {
            trade_meta_compiler::SymbolFactory(#fn_name)
        }
    }
}

// ── Metadata function ────────────────────────────────────────────────────────

fn gen_metadata(def: &SymbolDef) -> TokenStream2 {
    let vis = &def.vis;
    let fn_name = format_ident!("{}_metadata", to_snake_case(&def.name.to_string()));
    let name_str = def.name.to_string();

    let category = match def.category {
        Category::Monitor => quote!(trade_meta_compiler::SymbolCategory::Monitor),
        Category::Executor => quote!(trade_meta_compiler::SymbolCategory::Executor),
        Category::DataItem => quote!(trade_meta_compiler::SymbolCategory::DataItem),
        Category::Condition => quote!(trade_meta_compiler::SymbolCategory::Condition),
    };

    let returns_expr = match &def.returns {
        None => quote!(None),
        Some(r) if r.types.len() == 1 => {
            let ts = r.types[0].type_spec();
            quote!(Some(#ts))
        }
        Some(r) => {
            let specs: Vec<_> = r.types.iter().map(|t| t.type_spec()).collect();
            quote!(Some(trade_meta_compiler::TypeSpec::Tuple(
                vec![#(#specs),*]
            )))
        }
    };

    let param_specs: Vec<_> = def
        .params
        .iter()
        .map(|p| {
            let pname = p.name.to_string();
            match &p.trade_type {
                TradeType::Union(types) => {
                    let tspecs: Vec<_> = types.iter().map(|t| t.type_spec()).collect();
                    if p.required {
                        quote!(trade_meta_compiler::ParamSpec::required_multi(
                            #pname, vec![#(#tspecs),*]
                        ))
                    } else {
                        quote!(trade_meta_compiler::ParamSpec::optional_multi(
                            #pname, vec![#(#tspecs),*]
                        ))
                    }
                }
                TradeType::Alias(alias_name) => {
                    if p.required {
                        quote!(trade_meta_compiler::ParamSpec::required_multi(
                            #pname,
                            trade_meta_compiler::TypeAliasDef::lookup(#alias_name)
                                .unwrap_or_else(|| panic!("TypeAlias '{}' not registered", #alias_name))
                        ))
                    } else {
                        quote!(trade_meta_compiler::ParamSpec::optional_multi(
                            #pname,
                            trade_meta_compiler::TypeAliasDef::lookup(#alias_name)
                                .unwrap_or_else(|| panic!("TypeAlias '{}' not registered", #alias_name))
                        ))
                    }
                }
                other => {
                    let ts = other.type_spec();
                    if p.required {
                        quote!(trade_meta_compiler::ParamSpec::required(#pname, #ts))
                    } else {
                        quote!(trade_meta_compiler::ParamSpec::optional(#pname, #ts))
                    }
                }
            }
        })
        .collect();

    let ctx_interactions: Vec<_> = def
        .contexts
        .iter()
        .map(|c| {
            let proto = &c.protocol;
            match c.op {
                CtxOp::Produce => {
                    quote!(trade_meta_compiler::ContextInteraction::produce(#proto))
                }
                CtxOp::Need => {
                    quote!(trade_meta_compiler::ContextInteraction::need(#proto))
                }
                CtxOp::Consume => {
                    quote!(trade_meta_compiler::ContextInteraction::consume(#proto))
                }
            }
        })
        .collect();

    // 使用 leak 将 String 转为 &'static str（metadata 生命周期要求）
    quote! {
        #vis fn #fn_name() -> trade_meta_compiler::SymbolMetadata {
            trade_meta_compiler::SymbolMetadata {
                name: #name_str,
                returns: #returns_expr,
                params: vec![#(#param_specs),*],
                category: #category,
                contexts: vec![#(#ctx_interactions),*],
            }
        }
    }
}

// ── Handler trait ────────────────────────────────────────────────────────────

fn gen_handler_trait(def: &SymbolDef) -> TokenStream2 {
    let vis = &def.vis;
    let trait_name = format_ident!("{}Handler", def.name);

    // 构建参数列表
    let mut trait_params = Vec::new();

    // 1. 先放显式 params（required → T, optional → Option<T>）
    for p in &def.params {
        let pname = &p.name;
        let rtype = p.trade_type.rust_type();
        if p.required {
            trait_params.push(quote!(#pname: #rtype));
        } else {
            trait_params.push(quote!(#pname: Option<#rtype>));
        }
    }

    // 2. 再放 context 参数（need → &T, consume → Arc<T>）
    //    注意：Monitor 不接收 context 参数（它是源头，通过 channel 产出）
    if def.category != Category::Monitor {
        for c in &def.contexts {
            let proto_ident = format_ident!("{}", c.protocol.value());
            let ctx_ty = &c.ty;
            match c.op {
                CtxOp::Need => {
                    trait_params.push(quote!(#proto_ident: &#ctx_ty));
                }
                CtxOp::Consume => {
                    trait_params.push(quote!(#proto_ident: ::std::sync::Arc<#ctx_ty>));
                }
                CtxOp::Produce => {} // produce 不传给 handler，而是从返回值得到
            }
        }
    }

    // 3. Monitor → cancel token, 其他 → task context
    match def.category {
        Category::Monitor => {
            trait_params.push(quote!(
                cancel: trade_lang_core::CancellationToken
            ));
        }
        _ => {
            trait_params.push(quote!(
                ctx: &::std::sync::Arc<trade_lang_core::TradeTaskContext>
            ));
        }
    }

    // 确定返回类型和方法名
    let (method_name, return_type) = match def.category {
        Category::Monitor => {
            // Monitor 直接返回 Receiver<MonitorMessage>，impl 侧自行填充多个 context
            (format_ident!("start"), quote!(trade_lang_core::monitor_mpsc::Receiver<trade_lang_core::MonitorMessage>))
        }
        Category::Executor => {
            let inner = match &def.returns {
                None => quote!(()),
                Some(r) if r.types.len() == 1 => r.types[0].rust_type(),
                Some(r) => {
                    let types: Vec<_> = r.types.iter().map(|t| t.rust_type()).collect();
                    quote!((#(#types),*))
                }
            };
            let ret = quote!(trade_lang_core::ExecutorResult<#inner>);
            (format_ident!("execute"), ret)
        }
        Category::DataItem => {
            let ret = match &def.returns {
                Some(r) if r.types.len() == 1 => r.types[0].rust_type(),
                Some(r) => {
                    let types: Vec<_> = r.types.iter().map(|t| t.rust_type()).collect();
                    quote!((#(#types),*))
                }
                None => quote!(()),
            };
            (format_ident!("get"), ret)
        }
        Category::Condition => (format_ident!("evaluate"), quote!(bool)),
    };

    quote! {
        #[async_trait::async_trait]
        #vis trait #trait_name: Send + Sync {
            async fn #method_name(&self, #(#trait_params),*) -> #return_type;
        }
    }
}

// ── Adapter struct + impl ────────────────────────────────────────────────────

fn gen_adapter(def: &SymbolDef) -> TokenStream2 {
    let vis = &def.vis;
    let adapter_name = format_ident!("{}Adapter", def.name);
    let trait_name = format_ident!("{}Handler", def.name);

    // 参数提取代码
    let param_extractions = gen_param_extractions(def);
    let param_names: Vec<_> = def.params.iter().map(|p| &p.name).collect();

    // context 处理：context 缺失时的兜底行为，按 adapter 返回类型决定
    let (ctx_pre, ctx_args, ctx_post) = gen_context_handling(def, &def.category);

    match def.category {
        Category::Monitor => gen_monitor_adapter(
            vis,
            &adapter_name,
            &trait_name,
            &param_extractions,
            &param_names,
            &ctx_pre,
            &ctx_args,
            &ctx_post,
            def,
        ),
        Category::Executor => gen_executor_adapter(
            vis,
            &adapter_name,
            &trait_name,
            &param_extractions,
            &param_names,
            &ctx_pre,
            &ctx_args,
            &ctx_post,
            def,
        ),
        Category::DataItem => gen_data_item_adapter(
            vis,
            &adapter_name,
            &trait_name,
            &param_extractions,
            &param_names,
            &ctx_pre,
            &ctx_args,
            &ctx_post,
            def,
        ),
        Category::Condition => gen_condition_adapter(
            vis,
            &adapter_name,
            &trait_name,
            &param_extractions,
            &param_names,
            &ctx_pre,
            &ctx_args,
            &ctx_post,
            def,
        ),
    }
}

fn gen_param_extractions(def: &SymbolDef) -> Vec<TokenStream2> {
    def.params
        .iter()
        .map(|p| {
            let pname = &p.name;
            let pname_str = p.name.to_string();

            if p.trade_type.is_runtime_value_type() {
                // 联合类型/别名/Any — 直接传递 RuntimeValue
                if p.required {
                    quote! {
                        let #pname = args.get(#pname_str).cloned()
                            .unwrap_or(trade_meta_compiler::RuntimeValue::Unit);
                    }
                } else {
                    quote! {
                        let #pname = args.get(#pname_str).cloned();
                    }
                }
            } else {
                let rv_expr = quote!(v);
                let extract = p.trade_type.extract_from_rv(&rv_expr);
                if p.required {
                    quote! {
                        let #pname = args.get(#pname_str).map(|v| #extract).unwrap_or_default();
                    }
                } else {
                    quote! {
                        let #pname = args.get(#pname_str).map(|v| #extract);
                    }
                }
            }
        })
        .collect()
}

/// 返回 (pre_code, arg_tokens, post_code)
/// pre_code: 获取 context Arc / consume
/// arg_tokens: 传给 handler trait 方法的 context 参数
/// post_code: produce context 等后处理
fn gen_context_handling(
    def: &SymbolDef,
    category: &Category,
) -> (TokenStream2, Vec<TokenStream2>, TokenStream2) {
    let use_contexts: Vec<_> = def
        .contexts
        .iter()
        .filter(|c| matches!(c.op, CtxOp::Need))
        .collect();
    let consume_contexts: Vec<_> = def
        .contexts
        .iter()
        .filter(|c| matches!(c.op, CtxOp::Consume))
        .collect();

    let mut pre = quote!();
    let mut args = Vec::new();
    let post = quote!();

    // Consume contexts: remove from map, pass as Arc<T>
    for c in &consume_contexts {
        let proto_str = c.protocol.value();
        let proto_ident = format_ident!("{}", proto_str);
        let ctx_ty = &c.ty;
        let nf = make_context_not_found(category, &proto_str);
        pre = quote! {
            #pre
            let #proto_ident: ::std::sync::Arc<#ctx_ty> = match ctx.consume_context::<#ctx_ty>(#proto_str).await {
                Some(v) => v,
                None => { #nf }
            };
        };
        args.push(quote!(#proto_ident));
    }

    // Use contexts: get Arc clone (lock held only for HashMap lookup + Arc::clone)
    for c in &use_contexts {
        let proto_str = c.protocol.value();
        let proto_ident = format_ident!("{}", proto_str);
        let ctx_ty = &c.ty;
        let nf = make_context_not_found(category, &proto_str);
        pre = quote! {
            #pre
            let #proto_ident: ::std::sync::Arc<#ctx_ty> = match ctx.get_context::<#ctx_ty>(#proto_str).await {
                Some(v) => v,
                None => { #nf }
            };
        };
        // handler 接收 &T，从 Arc 自动 deref
        args.push(quote!(&#proto_ident));
    }

    // Produce contexts: handled per-category (from handler return value)

    (pre, args, post)
}

/// 根据 adapter 类型和 context 键名，生成“context 不存在”时的兜底代码
 fn make_context_not_found(category: &Category, proto_str: &str) -> TokenStream2 {
    let msg = format!("[context] '{}' not available", proto_str);
    match category {
        Category::Executor => quote! {
            log::warn!(#msg);
            ctx.signal_done();
            return None;
        },
        Category::DataItem => quote! {
            log::warn!(#msg);
            ctx.signal_done();
            return trade_meta_compiler::RuntimeValue::Unit;
        },
        Category::Condition => quote! {
            log::warn!(#msg);
            return false;
        },
        // Monitor 不使用 ctx_pre，这个分支不会被执行到
        Category::Monitor => quote! {},
    }
}

#[allow(clippy::too_many_arguments)]
fn gen_monitor_adapter(
    vis: &Visibility,
    adapter_name: &Ident,
    trait_name: &Ident,
    param_extractions: &[TokenStream2],
    param_names: &[&Ident],
    _ctx_pre: &TokenStream2,
    _ctx_args: &[TokenStream2],
    _ctx_post: &TokenStream2,
    def: &SymbolDef,
) -> TokenStream2 {
    // Monitor adapter: impl 直接返回 Receiver<MonitorMessage>，adapter 原样转发
    let body = quote! {
        self.0.start(#(#param_names,)* cancel).await
    };

    quote! {
        #vis struct #adapter_name<H: #trait_name>(pub H);

        #[async_trait::async_trait]
        impl<H: #trait_name + 'static> trade_lang_core::MonitorHandler for #adapter_name<H> {
            async fn start(
                &self,
                args: &::std::collections::HashMap<::std::string::String, trade_meta_compiler::RuntimeValue>,
                cancel: trade_lang_core::CancellationToken,
            ) -> trade_lang_core::monitor_mpsc::Receiver<trade_lang_core::MonitorMessage> {
                #(#param_extractions)*
                #body
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn gen_executor_adapter(
    vis: &Visibility,
    adapter_name: &Ident,
    trait_name: &Ident,
    param_extractions: &[TokenStream2],
    param_names: &[&Ident],
    ctx_pre: &TokenStream2,
    ctx_args: &[TokenStream2],
    ctx_post: &TokenStream2,
    def: &SymbolDef,
) -> TokenStream2 {
    let declared_return = match &def.returns {
        None => quote!(None),
        Some(r) if r.types.len() == 1 => {
            let ts = r.types[0].type_spec();
            quote!(Some(#ts))
        }
        Some(r) => {
            let specs: Vec<_> = r.types.iter().map(|t| t.type_spec()).collect();
            quote!(Some(trade_meta_compiler::TypeSpec::Tuple(
                vec![#(#specs),*]
            )))
        }
    };

    let (call_expr, return_expr) = match &def.returns {
        None => (
            quote! {
                match self.0.execute(#(#param_names,)* #(#ctx_args,)* ctx).await {
                    Ok(()) => {},
                    Err(e) => {
                        log::warn!("[executor] execute failed: {}, triggering finally", e);
                        ctx.signal_done();
                        return None;
                    }
                }
            },
            quote!(Some(trade_meta_compiler::RuntimeValue::Unit)),
        ),
        Some(r) if r.types.len() == 1 => {
            let wrap = r.types[0].wrap_in_rv(&quote!(__result));
            (
                quote! {
                    let __result = match self.0.execute(#(#param_names,)* #(#ctx_args,)* ctx).await {
                        Ok(v) => v,
                        Err(e) => {
                            log::warn!("[executor] execute failed: {}, triggering finally", e);
                            ctx.signal_done();
                            return None;
                        }
                    };
                },
                quote!(Some(#wrap)),
            )
        }
        Some(r) => {
            // Tuple destructuring: let (__r0, __r1, ...) = self.0.execute(...).await;
            let n = r.types.len();
            let ret_idents: Vec<_> = (0..n).map(|i| format_ident!("__r{}", i)).collect();
            let wraps: Vec<_> = r
                .types
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let ri = &ret_idents[i];
                    t.wrap_in_rv(&quote!(#ri))
                })
                .collect();
            (
                quote! {
                    let (#(#ret_idents),*) = match self.0.execute(#(#param_names,)* #(#ctx_args,)* ctx).await {
                        Ok(v) => v,
                        Err(e) => {
                            log::warn!("[executor] execute failed: {}, triggering finally", e);
                            ctx.signal_done();
                            return None;
                        }
                    };
                },
                quote!(Some(trade_meta_compiler::RuntimeValue::Tuple(
                    vec![#(#wraps),*]
                ))),
            )
        }
    };

    quote! {
        #vis struct #adapter_name<H: #trait_name>(pub H);

        #[async_trait::async_trait]
        impl<H: #trait_name + 'static> trade_lang_core::ExecutorHandler for #adapter_name<H> {
            fn declared_return_type(&self) -> Option<trade_meta_compiler::TypeSpec> {
                #declared_return
            }

            async fn execute(
                &self,
                args: &::std::collections::HashMap<::std::string::String, trade_meta_compiler::RuntimeValue>,
                ctx: &::std::sync::Arc<trade_lang_core::TradeTaskContext>,
            ) -> Option<trade_meta_compiler::RuntimeValue> {
                #(#param_extractions)*
                #ctx_pre
                #call_expr
                #ctx_post
                #return_expr
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn gen_data_item_adapter(
    vis: &Visibility,
    adapter_name: &Ident,
    trait_name: &Ident,
    param_extractions: &[TokenStream2],
    param_names: &[&Ident],
    ctx_pre: &TokenStream2,
    ctx_args: &[TokenStream2],
    ctx_post: &TokenStream2,
    def: &SymbolDef,
) -> TokenStream2 {
    let declared_return = match &def.returns {
        Some(r) if r.types.len() == 1 => r.types[0].type_spec(),
        _ => quote!(trade_meta_compiler::TypeSpec::Any),
    };

    let return_wrap = match &def.returns {
        Some(r) if r.types.len() == 1 => r.types[0].wrap_in_rv(&quote!(__result)),
        _ => quote!(__result),
    };

    quote! {
        #vis struct #adapter_name<H: #trait_name>(pub H);

        #[async_trait::async_trait]
        impl<H: #trait_name + 'static> trade_lang_core::DataItemHandler for #adapter_name<H> {
            fn declared_return_type(&self) -> trade_meta_compiler::TypeSpec {
                #declared_return
            }

            async fn get(
                &self,
                args: &::std::collections::HashMap<::std::string::String, trade_meta_compiler::RuntimeValue>,
                ctx: &::std::sync::Arc<trade_lang_core::TradeTaskContext>,
            ) -> trade_meta_compiler::RuntimeValue {
                #(#param_extractions)*
                #ctx_pre
                let __result = self.0.get(#(#param_names,)* #(#ctx_args,)* ctx).await;
                #ctx_post
                #return_wrap
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn gen_condition_adapter(
    vis: &Visibility,
    adapter_name: &Ident,
    trait_name: &Ident,
    param_extractions: &[TokenStream2],
    param_names: &[&Ident],
    ctx_pre: &TokenStream2,
    ctx_args: &[TokenStream2],
    ctx_post: &TokenStream2,
    _def: &SymbolDef,
) -> TokenStream2 {
    quote! {
        #vis struct #adapter_name<H: #trait_name>(pub H);

        #[async_trait::async_trait]
        impl<H: #trait_name + 'static> trade_lang_core::ConditionHandler for #adapter_name<H> {
            async fn evaluate(
                &self,
                args: &::std::collections::HashMap<::std::string::String, trade_meta_compiler::RuntimeValue>,
                ctx: &::std::sync::Arc<trade_lang_core::TradeTaskContext>,
            ) -> bool {
                #(#param_extractions)*
                #ctx_pre
                let __result = self.0.evaluate(#(#param_names,)* #(#ctx_args,)* ctx).await;
                #ctx_post
                __result
            }
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Entry point
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// 定义一个 trade-lang 符号，同时生成：
///   - `{snake_case}_metadata()` → `SymbolMetadata`
///   - `{Name}Handler` trait（类型化的 handler 接口）
///   - `{Name}Adapter<H>` 结构体（桥接到通用 Handler trait）
#[proc_macro]
pub fn define_symbol(input: TokenStream) -> TokenStream {
    let def = syn::parse_macro_input!(input as SymbolDef);
    generate(&def).into()
}
