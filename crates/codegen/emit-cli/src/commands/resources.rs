//! Resource grouping and the per-resource `Subcommand` enum
//! (`src/commands/{module}.rs` in the generated CLI): one file per OpenAPI
//! tag, each holding one variant per endpoint plus a `dispatch` function and
//! a `delete_by_id` used by the top-level `reset` command.

use std::collections::{BTreeMap, HashSet};

use heck::{ToSnakeCase, ToUpperCamelCase};
use proc_macro2::TokenStream;
use quote::quote;
use tokyo_ir::cli_behavior::CliDispatchGroup;
use tokyo_ir::http::{Endpoint, HttpMethod};

use crate::naming::{rust_field_identifier, rust_identifier, rust_variant_identifier};
use crate::types::TypeCatalog;

use super::endpoint::{
    DispatchEndpoint, DispatchMemberEndpoint, render_dispatch_endpoint, render_endpoint,
};

pub struct ResolvedDispatchGroup<'a> {
    pub group: &'a CliDispatchGroup,
    pub members: Vec<&'a Endpoint>,
}

/// Groups endpoints by their first tag (OpenAPI's own grouping mechanism),
/// falling back to a single `default` resource for untagged operations —
/// a `BTreeMap` key keeps resource and generated-file ordering deterministic.
pub fn resource_groups(endpoints: &[Endpoint]) -> Vec<(String, Vec<&Endpoint>)> {
    let mut groups: BTreeMap<String, Vec<&Endpoint>> = BTreeMap::new();
    for endpoint in endpoints {
        let tag = endpoint
            .tags
            .first()
            .cloned()
            .unwrap_or_else(|| "default".to_string());
        groups.entry(tag).or_default().push(endpoint);
    }
    groups.into_iter().collect()
}

/// Names built into `Command` (see `render_generated_cli_source_tokens`) that a resource's derived
/// variant name must not collide with.
const RESERVED_COMMAND_NAMES: &[&str] = &[
    "Api",
    "Auth",
    "Profile",
    "Env",
    "Start",
    "Schema",
    "Completions",
    "Run",
    "Reset",
];

/// Resolves every resource's module/enum names once, up front, avoiding
/// collisions with the CLI's own built-in top-level commands and with each
/// other. A real OpenAPI tag can legitimately be named "Auth" (as chrt-fastapi's
/// is) — colliding with this CLI's own `auth` profile-management subcommand
/// silently produced two `Command::Auth` variants instead of a clean
/// diagnostic, so this needs handling, not luck. Mirrors the collision-avoidance
/// shape of `naming::type_identifiers_by_type_id`, one level up (resources, not types).
pub fn resolve_resource_names(
    groups: &[(String, Vec<&Endpoint>)],
) -> Vec<(syn::Ident, syn::Ident)> {
    let mut used: HashSet<String> = RESERVED_COMMAND_NAMES
        .iter()
        .map(|name| name.to_string())
        .collect();
    groups
        .iter()
        .map(|(tag, _)| {
            let mut candidate = tag.to_upper_camel_case();
            if candidate.is_empty() {
                candidate = "Resource".to_string();
            }
            if used.contains(&candidate) {
                let mut suffixed = format!("{candidate}Resource");
                let mut n = 2;
                while used.contains(&suffixed) {
                    suffixed = format!("{candidate}Resource{n}");
                    n += 1;
                }
                candidate = suffixed;
            }
            used.insert(candidate.clone());
            (
                rust_identifier(&candidate.to_snake_case()),
                rust_identifier(&format!("{candidate}Command")),
            )
        })
        .collect()
}

/// Renders one resource's `src/commands/{module}.rs`: a `clap::Subcommand` enum
/// with one struct-variant per endpoint, and a `dispatch` function matching on
/// it to build+send the request and print the response.
pub fn render_resource(
    enum_name: &syn::Ident,
    endpoints: &[&Endpoint],
    dispatch_groups: &[ResolvedDispatchGroup<'_>],
    catalog: &TypeCatalog,
    resource_name: &str,
) -> TokenStream {
    let mut variants = Vec::new();
    let mut arms = Vec::new();
    let mut dispatch_identity_arms = Vec::new();
    // The first single-ID `DELETE` in this resource (no query/header/body
    // needed) becomes `delete_by_id`, letting `reset` undo whatever was
    // created here without any spec-level create/delete pairing metadata.
    let mut delete_by_id_target: Option<(syn::Ident, syn::Ident)> = None;
    for endpoint in endpoints {
        // `x-tokyo-cli-ignore`: no dedicated command, still reachable via
        // the `api` escape hatch.
        if endpoint
            .cli
            .as_ref()
            .is_some_and(|overrides| overrides.ignore)
        {
            continue;
        }
        if delete_by_id_target.is_none()
            && endpoint.method == HttpMethod::Delete
            && endpoint.path_parameters.len() == 1
            && endpoint.query_parameters.is_empty()
            && endpoint.headers.is_empty()
            && endpoint.request_body.is_none()
        {
            delete_by_id_target = Some((
                rust_variant_identifier(&endpoint.name),
                rust_field_identifier(&endpoint.path_parameters[0].name),
            ));
        }
        let (variant, arm) = render_endpoint(endpoint, enum_name, catalog, resource_name);
        variants.push(variant);
        arms.push(arm);
    }
    for resolved in dispatch_groups {
        let default_index = resolved
            .group
            .members
            .iter()
            .position(|member| member.name == resolved.group.default_member)
            .expect("dispatch groups are validated");
        let mut public_endpoint = resolved.members[default_index].clone();
        public_endpoint.name = resolved.group.name.clone();
        public_endpoint.summary = resolved.group.description.clone();
        public_endpoint.docs = resolved.group.description.clone();
        public_endpoint.cli = None;
        let dispatch = DispatchEndpoint {
            default_member: &resolved.group.default_member,
            members: resolved
                .group
                .members
                .iter()
                .zip(&resolved.members)
                .map(|(member, endpoint)| DispatchMemberEndpoint {
                    name: &member.name,
                    view: member.view.as_deref(),
                    identity: &member.identity,
                    endpoint,
                })
                .collect(),
        };
        let (variant, arm) = render_dispatch_endpoint(
            &public_endpoint,
            enum_name,
            catalog,
            resource_name,
            &dispatch,
        );
        let variant_name = rust_variant_identifier(&public_endpoint.name);
        if resolved
            .group
            .members
            .iter()
            .any(|member| member.view.is_some())
        {
            dispatch_identity_arms
                .push(quote! { #enum_name::#variant_name { __view, .. } => __view.is_none(), });
        } else {
            dispatch_identity_arms.push(quote! { #enum_name::#variant_name { .. } => true, });
        }
        variants.push(variant);
        arms.push(arm);
    }
    let delete_by_id_fn = match delete_by_id_target {
        Some((variant_name, field)) => quote! {
            pub fn delete_by_id(__id: &str) -> Option<#enum_name> {
                Some(#enum_name::#variant_name { #field: __id.parse().ok()? })
            }
        },
        None => quote! {
            pub fn delete_by_id(_id: &str) -> Option<#enum_name> {
                None
            }
        },
    };
    let identity_arg =
        (!dispatch_groups.is_empty()).then(|| quote! { __identity: Option<&serde_json::Value>, });
    let requires_identity_fn = (!dispatch_groups.is_empty()).then(|| {
        quote! {
            pub fn requires_identity(__command: &#enum_name) -> bool {
                match __command {
                    #(#dispatch_identity_arms)*
                    _ => false,
                }
            }
        }
    });
    // `__`-prefixed throughout this function's own scaffolding — parameters,
    // locals, everything — not just the query/header accumulators: chrt-fastapi
    // (a real production spec this was stress-tested against) has an actual
    // operation with a query parameter named `query`, which silently shadowed
    // a same-named local. A match arm's field bindings can shadow *any* outer
    // name, including a `dispatch` parameter, so the fix has to be "nothing
    // we control can ever collide with an OpenAPI-derived identifier," not a
    // one-off rename.
    let file = quote! {
        #[derive(Debug, clap::Subcommand)]
        pub enum #enum_name {
            #(#variants)*
        }

        pub fn dispatch(
            __command: &#enum_name,
            __client: &crate::client::Client,
            __output: &crate::output::OutputOptions,
            #identity_arg
        ) -> Result<(), crate::error::ClientError> {
            match __command {
                #(#arms)*
            }
        }

        #requires_identity_fn

        #delete_by_id_fn
    };
    file
}
