//! 从宿主类型和操作声明生成独立 SDK 合同；普通测试阻止两侧声明漂移

use std::{collections::BTreeMap, path::Path};

use quote::quote;
use syn::{Item, Type, parse::Parse, parse_quote, visit_mut::VisitMut};

struct Operation {
    name: syn::Ident,
    method: syn::Ident,
    input: Type,
}

impl Parse for Operation {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let name = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let method = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let arguments = input.parse()?;
        // 后续模式和参数表达式只属于宿主调用，不进入线协议
        input.parse::<syn::Token![,]>()?;
        let _ = syn::Pat::parse_single(input)?;
        while !input.is_empty() {
            input.parse::<syn::Token![,]>()?;
            if !input.is_empty() {
                let _ = input.parse::<syn::Expr>()?;
            }
        }
        Ok(Self {
            name,
            method,
            input: arguments,
        })
    }
}

fn source(root: &Path, path: &str) -> syn::File {
    syn::parse_file(&std::fs::read_to_string(root.join(path)).unwrap()).unwrap()
}

// SDK 保留 wire 值，不依赖宿主的领域构造器、时区库或价格校验实现
struct WireTypes;
impl VisitMut for WireTypes {
    fn visit_type_mut(&mut self, ty: &mut Type) {
        if let Type::Path(path) = ty {
            assert!(path.qself.is_none(), "服务合同不支持关联类型");
            if path
                .path
                .segments
                .first()
                .is_some_and(|segment| segment.ident == "serde_json")
            {
                syn::visit_mut::visit_type_mut(self, ty);
                return;
            }
            let last = path.path.segments.last().unwrap().clone();
            *ty = match last.ident.to_string().as_str() {
                "DateTime" | "Tz" | "TokenPrice" | "AdminApiKey" => parse_quote!(String),
                "OpaqueProviderData" => parse_quote!(serde_json::Map<String, serde_json::Value>),
                _ => Type::Path(syn::TypePath {
                    attrs: std::mem::take(&mut path.attrs),
                    qself: None,
                    path: last.into(),
                }),
            };
        }
        syn::visit_mut::visit_type_mut(self, ty);
    }
}

fn contract_type(mut item: Item, name: &str) -> Item {
    let (attrs, ident, fields, comparable) = match &mut item {
        Item::Struct(item) => {
            item.vis = parse_quote!(pub);
            let comparable = !matches!(
                name,
                "RuntimeSettings"
                    | "ReplaceRuntimeSettings"
                    | "AdminApiKeyMutation"
                    | "RegeneratedAdminApiKey"
            );
            (
                &mut item.attrs,
                &mut item.ident,
                Some(&mut item.fields),
                comparable,
            )
        }
        Item::Enum(item) => (&mut item.attrs, &mut item.ident, None, true),
        _ => panic!("公开合同必须是 struct 或 enum"),
    };
    *ident = syn::parse_str(name).unwrap();
    attrs.retain(|attr| attr.path().is_ident("serde"));
    attrs.insert(
        0,
        if name == "SmartSchedulingConfig" {
            parse_quote!(#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)])
        } else if comparable {
            parse_quote!(#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)])
        } else {
            parse_quote!(#[derive(Clone, Serialize, Deserialize)])
        },
    );
    if let Some(fields) = fields {
        for field in fields {
            field.vis = parse_quote!(pub);
        }
    }
    WireTypes.visit_item_mut(&mut item);
    item
}

#[test]
fn sdk_settings_contract_matches_host_declarations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut types = Vec::new();
    let mut replacement_fields = Vec::new();
    for (path, names) in [
        (
            "src/model/settings.rs",
            &[
                "RuntimeSettings",
                "ReplaceRuntimeSettings",
                "AdminApiKeyMutation",
                "RegeneratedAdminApiKey",
            ][..],
        ),
        (
            "src/model/mod.rs",
            &["MutationActor", "MutationContext"][..],
        ),
        (
            "../gateway-core/src/account/location.rs",
            &["RequestLocation"][..],
        ),
        (
            "../gateway-core/src/account/smart_scheduling.rs",
            &["SmartSchedulingValues"][..],
        ),
        (
            "../gateway-core/src/metering/pricing.rs",
            &["TokenPriceOverride", "ModelPriceOverride"][..],
        ),
        (
            "src/model/pricing.rs",
            &[
                "PricingCatalog",
                "PricingSyncPreview",
                "SyncPricing",
                "PricingChange",
                "UpdatePricing",
            ][..],
        ),
    ] {
        let items = source(root, path).items;
        for name in names {
            let item = items
                .iter()
                .find(|item| match item {
                    Item::Struct(item) => item.ident == name,
                    Item::Enum(item) => item.ident == name,
                    _ => false,
                })
                .unwrap_or_else(|| panic!("缺少合同类型 {name}"));
            if let Item::Struct(item) = item
                && item.ident == "ReplaceRuntimeSettings"
            {
                replacement_fields = item
                    .fields
                    .iter()
                    .map(|field| field.ident.clone().unwrap())
                    .collect();
            }
            types.push(contract_type(
                item.clone(),
                if *name == "SmartSchedulingValues" {
                    "SmartSchedulingConfig"
                } else {
                    name
                },
            ));
        }
    }

    let service = source(root, "src/use_case/settings.rs")
        .items
        .into_iter()
        .find_map(|item| match item {
            Item::Trait(item) if item.ident == "SettingsService" => Some(item),
            _ => None,
        })
        .unwrap();
    let outputs: BTreeMap<_, _> = service
        .items
        .into_iter()
        .filter_map(|item| match item {
            syn::TraitItem::Fn(method) => {
                let syn::ReturnType::Type(_, output) = method.sig.output else {
                    panic!("服务必须返回 Result")
                };
                let Type::Path(result) = *output else {
                    panic!("服务必须返回 Result")
                };
                let segment = result.path.segments.last().unwrap();
                assert_eq!(segment.ident, "Result");
                let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
                    panic!("Result 缺少类型参数")
                };
                let syn::GenericArgument::Type(mut output) = args.args.first().unwrap().clone()
                else {
                    panic!("Result 缺少输出类型")
                };
                WireTypes.visit_type_mut(&mut output);
                Some((method.sig.ident.to_string(), output))
            }
            _ => None,
        })
        .collect();
    let register = source(root, "src/service/settings.rs")
        .items
        .into_iter()
        .find_map(|item| match item {
            Item::Fn(item) if item.sig.ident == "register" => Some(item),
            _ => None,
        })
        .unwrap();
    let operations: Vec<_> = register
        .block
        .stmts
        .into_iter()
        .filter_map(|stmt| {
            let syn::Stmt::Macro(stmt) = stmt else {
                return None;
            };
            if !stmt.mac.path.is_ident("register") {
                return None;
            }
            let Operation {
                name,
                method,
                mut input,
            } = syn::parse2(stmt.mac.tokens).unwrap();
            WireTypes.visit_type_mut(&mut input);
            let output = outputs.get(&method.to_string()).unwrap();
            let operation = format!("settings.{method}");
            Some(quote! {
                pub struct #name;
                impl Operation for #name {
                    const NAME: &'static str = #operation;
                    type Input = #input;
                    type Output = #output;
                }
            })
        })
        .collect();
    assert_eq!(operations.len(), outputs.len(), "公开服务方法必须完整声明");
    let assignments = replacement_fields.iter().map(|field| match field.to_string().as_str() {
        "expected_revision" => quote!(expected_revision: settings.config_revision),
        "request_profile_updates" => quote!(request_profile_updates: settings.request_profiles.into_iter().map(|(provider, value)| (provider, Some(value))).collect()),
        _ => quote!(#field: settings.#field),
    });
    let generated = quote! {
        //! 宿主设置服务的操作标识与请求、响应数据合同
        //!
        //! 从宿主设置类型与 service/settings.rs 生成；更新命令见 SDK 维护说明

        use super::Operation;
        use serde::{Deserialize, Serialize};
        use std::{collections::{BTreeMap, BTreeSet}, num::NonZeroU64};
        pub type ProviderRequestProfiles = BTreeMap<String, serde_json::Map<String, serde_json::Value>>;
        pub type ProviderRequestProfileUpdates = BTreeMap<String, Option<serde_json::Map<String, serde_json::Value>>>;
        pub type ModelMappings = BTreeMap<String, String>;
        pub type Revision = NonZeroU64;
        pub type RotationStrategy = String;
        pub type PricingOverrides = BTreeMap<String, BTreeMap<String, ModelPriceOverride>>;
        #(#types)*
        #(#operations)*
        impl From<RuntimeSettings> for ReplaceRuntimeSettings {
            fn from(settings: RuntimeSettings) -> Self {
                Self { #(#assignments),* }
            }
        }
    };
    let output = root.join("../gateway-plugin/sdk/src/call/services/settings.rs");
    let expected = prettyplease::unparse(&syn::parse2::<syn::File>(generated).unwrap());
    if std::env::var_os("CPR_UPDATE_SERVICE_CONTRACT").is_some() {
        std::fs::write(&output, &expected).unwrap();
    }
    let actual = syn::parse_file(&std::fs::read_to_string(output).unwrap()).unwrap();
    assert!(
        prettyplease::unparse(&actual) == expected,
        "SDK 服务合同已过期：CPR_UPDATE_SERVICE_CONTRACT=1 cargo test -p gateway-admin --test main sdk_settings_contract_matches_host_declarations"
    );
}
