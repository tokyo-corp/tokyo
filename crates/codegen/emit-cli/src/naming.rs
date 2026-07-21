use std::collections::{HashMap, HashSet};

use heck::{ToKebabCase, ToSnakeCase, ToUpperCamelCase};
use tokyo_ir::id::TypeId;
use tokyo_ir::types::TypeDeclaration;

/// Rust identifiers that collide with a prelude item raw identifiers can't fix
/// (`r#String` parses but still shadows the real `String`). Anything else
/// colliding with a keyword goes through [`ident`]'s raw-identifier escape
/// instead of a rename.
const PRELUDE_COLLISIONS: &[&str] = &[
    "String", "Vec", "Result", "Option", "Box", "Some", "None", "Ok", "Err",
];

pub struct PackageNames {
    pub cargo_package: String,
    pub command: String,
}

/// Derives safe Cargo and executable names from a configured package name.
pub fn derive_package_and_command_names(package_name: &str) -> PackageNames {
    let (cargo_source, command_source) = package_name
        .strip_prefix('@')
        .and_then(|scoped| scoped.split_once('/'))
        .filter(|(scope, name)| !scope.is_empty() && !name.is_empty())
        .map_or((package_name.to_string(), package_name), |(scope, name)| {
            (format!("{scope}-{name}"), name)
        });

    PackageNames {
        cargo_package: cargo_sanitize_package_name_component(&cargo_source),
        command: command_sanitize_package_name_component(command_source),
    }
}

fn cargo_sanitize_package_name_component(name: &str) -> String {
    sanitize_package_name_component(name, "generated-cli")
}

fn command_sanitize_package_name_component(name: &str) -> String {
    sanitize_package_name_component(name, "generated-cli")
}

fn sanitize_package_name_component(name: &str, fallback: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut previous_separator = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            result.push(character.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator && !result.is_empty() {
            result.push('-');
            previous_separator = true;
        }
    }
    while result.ends_with(['-', '_']) {
        result.pop();
    }
    if result.is_empty() {
        fallback.to_string()
    } else {
        result
    }
}

/// Parses `name` as a Rust identifier, escaping keyword collisions (`type`,
/// `match`, ...) as a raw identifier rather than renaming. Raw identifiers need
/// no registry entry and can't collide with anything else.
pub fn rust_identifier(name: &str) -> syn::Ident {
    syn::parse_str::<syn::Ident>(name).unwrap_or_else(|_| {
        syn::parse_str::<syn::Ident>(&format!("r#{name}")).expect("raw identifier")
    })
}

/// Maps every declared type's [`TypeId`] to the Rust identifier it should
/// render as, resolving std/prelude collisions once up front. Every renderer
/// consults this map instead of re-deriving a name from `decl.name`, so a
/// rename here needs no separate reference-rewrite pass.
pub fn type_identifiers_by_type_id(types: &[TypeDeclaration]) -> HashMap<TypeId, syn::Ident> {
    let mut used_names: HashSet<String> = HashSet::new();
    let mut idents_by_type_id = HashMap::new();
    for decl in types {
        let mut candidate = decl.name.clone();
        if PRELUDE_COLLISIONS.contains(&candidate.as_str()) || used_names.contains(&candidate) {
            let mut suffixed = format!("{candidate}Value");
            let mut suffix_counter = 2;
            while used_names.contains(&suffixed) {
                suffixed = format!("{candidate}Value{suffix_counter}");
                suffix_counter += 1;
            }
            candidate = suffixed;
        }
        used_names.insert(candidate.clone());
        idents_by_type_id.insert(decl.id.clone(), rust_identifier(&candidate));
    }
    idents_by_type_id
}

/// Struct/enum field name: IR field names are lowerCamelCase (chosen for TS),
/// so this converts to Rust's snake_case convention independent of the wire
/// name, which is preserved separately via `#[serde(rename)]`.
pub fn rust_field_identifier(field_name: &str) -> syn::Ident {
    rust_identifier(&field_name.to_snake_case())
}

/// Enum variant / union arm name.
pub fn rust_variant_identifier(name: &str) -> syn::Ident {
    rust_identifier(&name.to_upper_camel_case())
}

/// A resource's human-facing name, e.g. for `reset`'s per-resource matching
/// and the `schema` command's resource listing — kept in one place so both
/// stay in sync.
pub fn resource_display_name(tag: &str) -> String {
    tag.to_kebab_case()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_npm_names_produce_cargo_and_command_names() {
        let names = derive_package_and_command_names("@example/sdk");
        assert_eq!(names.cargo_package, "example-sdk");
        assert_eq!(names.command, "sdk");
    }

    #[test]
    fn existing_cargo_safe_names_are_retained() {
        let names = derive_package_and_command_names("generated-cli");
        assert_eq!(names.cargo_package, "generated-cli");
        assert_eq!(names.command, "generated-cli");
    }
}
