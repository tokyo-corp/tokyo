//! Renders one endpoint's `clap::Subcommand` variant and dispatch arm: path/
//! query/header args, request-body handling (flattened flags vs. `--body`
//! file), auth mode, and response decoding.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use tokyo_ir::auth::{AuthSchemeKind, CommandAccess};
use tokyo_ir::http::{
    BodyEncoding, CliOverrides, Endpoint, HttpMethod, Parameter, QuerySerialization,
    ResponseEncoding, StreamingKind, UrlResolution,
};
use tokyo_ir::types::{ObjectType, PrimitiveType, TypeRef, TypeShape};

use crate::naming::{rust_field_identifier, rust_variant_identifier};
use crate::types::{TypeCatalog, render_type_ref};

pub(super) struct DispatchEndpoint<'a> {
    pub default_member: &'a str,
    pub members: Vec<DispatchMemberEndpoint<'a>>,
}

pub(super) struct DispatchMemberEndpoint<'a> {
    pub name: &'a str,
    pub view: Option<&'a str>,
    pub identity: &'a std::collections::BTreeMap<String, String>,
    pub endpoint: &'a Endpoint,
}

struct BoundArg {
    field: TokenStream,
    pattern: syn::Ident,
    push: TokenStream,
}

/// Renders a single OpenAPI-sourced description as a `#[doc = ...]` attribute,
/// or nothing when the spec didn't provide one. clap-derive reads doc
/// comments for `--help` text on both command variants and their flags, so
/// this is the only wiring needed to surface spec prose in the generated CLI.
fn render_doc_attribute_from_description(text: Option<&str>) -> TokenStream {
    match text {
        Some(text) if !text.trim().is_empty() => quote! { #[doc = #text] },
        _ => TokenStream::new(),
    }
}

/// Combines an operation's `summary` and longer `description` into one doc
/// comment: summary first (clap's short `--help` line), a blank line, then
/// the full description (clap's long `--help` body) when it differs from the
/// summary.
fn endpoint_render_doc_attribute_from_description(
    summary: Option<&str>,
    docs: Option<&str>,
    access: CommandAccess,
) -> TokenStream {
    let marker = match access {
        CommandAccess::Public => "[public]",
        CommandAccess::Optional => "[authentication optional]",
        CommandAccess::Authenticated => "[authentication required]",
    };
    let text = match (summary, docs) {
        (Some(summary), Some(docs)) if summary.trim() != docs.trim() => {
            Some(format!("{summary} {marker}\n\n{docs}"))
        }
        (Some(summary), _) => Some(format!("{summary} {marker}")),
        (None, Some(docs)) => Some(format!("{docs} {marker}")),
        (None, None) => Some(marker.to_string()),
    };
    render_doc_attribute_from_description(text.as_deref())
}

pub(super) fn render_endpoint(
    endpoint: &Endpoint,
    enum_name: &syn::Ident,
    catalog: &TypeCatalog,
    resource_name: &str,
) -> (TokenStream, TokenStream) {
    render_endpoint_command_variant_and_dispatch_arm(
        endpoint,
        enum_name,
        catalog,
        resource_name,
        None,
    )
}

pub(super) fn render_dispatch_endpoint(
    endpoint: &Endpoint,
    enum_name: &syn::Ident,
    catalog: &TypeCatalog,
    resource_name: &str,
    dispatch: &DispatchEndpoint<'_>,
) -> (TokenStream, TokenStream) {
    render_endpoint_command_variant_and_dispatch_arm(
        endpoint,
        enum_name,
        catalog,
        resource_name,
        Some(dispatch),
    )
}

fn render_endpoint_command_variant_and_dispatch_arm(
    endpoint: &Endpoint,
    enum_name: &syn::Ident,
    catalog: &TypeCatalog,
    resource_name: &str,
    dispatch: Option<&DispatchEndpoint<'_>>,
) -> (TokenStream, TokenStream) {
    let variant_name = rust_variant_identifier(&endpoint.name);

    let path_args: Vec<BoundArg> = endpoint
        .path_parameters
        .iter()
        .map(|param| {
            let field = rust_field_identifier(&param.name);
            let ty =
                render_parameter_arg_type(&strip_nullable_type_ref_wrapper(&param.r#type), catalog);
            // A default here lets an agent run e.g. `pet get` with no ID and
            // land on the spec's own example resource — handy for a first
            // smoke test before any real data exists.
            let field_attr = match param.example.as_ref().and_then(example_default_value) {
                Some(default) => quote! { #[arg(default_value = #default)] },
                None => TokenStream::new(),
            };
            let doc = render_doc_attribute_from_description(param.docs.as_deref());
            BoundArg {
                field: quote! { #doc #field_attr #field: #ty, },
                pattern: field,
                push: TokenStream::new(),
            }
        })
        .collect();

    let query_args: Vec<BoundArg> = endpoint
        .query_parameters
        .iter()
        .map(|param| render_query_arg(param, catalog))
        .collect();
    let header_args: Vec<BoundArg> = endpoint
        .headers
        .iter()
        .map(|param| render_header_arg(param, catalog))
        .collect();
    let cookie_args: Vec<BoundArg> = endpoint
        .cookies
        .iter()
        .map(|param| render_cookie_arg(param, catalog))
        .collect();

    let body = endpoint.request_body.as_ref().map(|body_ref| {
        render_request_body_input_mode(body_ref, endpoint.request_body_encoding, catalog)
    });

    let mut fields = Vec::new();
    let mut pattern_fields = Vec::new();
    for arg in path_args
        .iter()
        .chain(&query_args)
        .chain(&header_args)
        .chain(&cookie_args)
    {
        fields.push(arg.field.clone());
        pattern_fields.push(arg.pattern.clone());
    }
    match &body {
        Some(RequestBody::Flattened { args, .. }) => {
            for arg in args {
                fields.push(arg.field.clone());
                pattern_fields.push(arg.pattern.clone());
            }
        }
        Some(RequestBody::File { .. }) => {
            fields.push(quote! {
                /// Read the request body from a file, or from stdin with `-`.
                #[arg(long = "body", value_name = "FILE", group = "__body_input")]
                __body_file: Option<String>,
            });
            fields.push(quote! {
                /// Supply the request body as inline JSON.
                #[arg(long = "body-json", value_name = "JSON", group = "__body_input")]
                __body_json: Option<String>,
            });
            fields.push(quote! {
                /// Set a request-body field as `path=value`; repeat for more fields.
                #[arg(short = 'f', long = "field", value_name = "PATH=VALUE", group = "__body_input")]
                __body_fields: Vec<String>,
            });
            pattern_fields.push(format_ident!("__body_file"));
            pattern_fields.push(format_ident!("__body_json"));
            pattern_fields.push(format_ident!("__body_fields"));
        }
        None => {}
    }
    if let Some(dispatch) = dispatch {
        let views = dispatch
            .members
            .iter()
            .filter_map(|member| member.view)
            .collect::<Vec<_>>();
        if !views.is_empty() {
            fields.push(quote! {
                /// Select a specific member projection instead of identity-based routing.
                #[arg(long, value_parser = [#(#views),*])]
                __view: Option<String>,
            });
            pattern_fields.push(format_ident!("__view"));
        }
    }

    let command_attrs = render_cli_overrides(endpoint.cli.as_ref());
    let body_group = matches!(body, Some(RequestBody::File { .. })).then(|| {
        quote! {
            #[command(group(
                clap::ArgGroup::new("__body_input").required(true).multiple(false)
            ))]
        }
    });
    let doc = endpoint_render_doc_attribute_from_description(
        endpoint.summary.as_deref(),
        endpoint.docs.as_deref(),
        CommandAccess::from_requirements(&endpoint.auth),
    );
    let variant = quote! {
        #doc
        #command_attrs
        #body_group
        #variant_name {
            #(#fields)*
        },
    };

    let query_pushes = query_args.iter().map(|arg| &arg.push);
    let header_pushes = header_args.iter().chain(&cookie_args).map(|arg| &arg.push);
    let query_mut = (!query_args.is_empty()).then(|| quote! { mut });
    let headers_mut = (!header_args.is_empty() || !cookie_args.is_empty()).then(|| quote! { mut });

    let request_body_expr = render_request_body_expr(endpoint, body.as_ref(), catalog);
    let execute = match dispatch {
        Some(dispatch) => render_dispatch_execution(dispatch, catalog, resource_name),
        None => render_endpoint_execution(endpoint, catalog, resource_name),
    };

    let arm = quote! {
        #enum_name::#variant_name { #(#pattern_fields),* } => {
            let #query_mut __query: Vec<crate::client::QueryParameter> = Vec::new();
            #(#query_pushes)*
            let #headers_mut __headers: Vec<(String, String)> = Vec::new();
            #(#header_pushes)*
            let __request_body = { #request_body_expr };
            #execute
            Ok(())
        }
    };
    (variant, arm)
}

fn render_endpoint_execution(
    endpoint: &Endpoint,
    catalog: &TypeCatalog,
    resource_name: &str,
) -> TokenStream {
    let method = render_http_method_token(endpoint.method);
    let auth = render_auth_mode(endpoint);
    let target_setup = render_request_target(endpoint, catalog);
    let accept_value = endpoint.accept.clone().or_else(|| {
        endpoint
            .responses
            .iter()
            .filter(|(status, _)| (200..300).contains(*status))
            .find_map(|(_, response)| response.media_type.clone())
    });
    let accept = accept_value
        .as_deref()
        .map_or_else(|| quote! { None }, |value| quote! { Some(#value) });
    let request_media_type = endpoint
        .request_media_type
        .as_deref()
        .map_or_else(|| quote! { None }, |value| quote! { Some(#value) });
    let wildcard_error_media_type = endpoint
        .wildcard_error_media_type
        .as_deref()
        .map_or_else(|| quote! { None }, |value| quote! { Some(#value) });
    let media = RenderedMedia {
        request: &request_media_type,
        wildcard_error: &wildcard_error_media_type,
    };
    let execute = render_request_execution(
        endpoint,
        catalog,
        resource_name,
        &method,
        &auth,
        &accept,
        &media,
    );
    quote! {
        #target_setup
        #execute
    }
}

fn render_dispatch_execution(
    dispatch: &DispatchEndpoint<'_>,
    catalog: &TypeCatalog,
    resource_name: &str,
) -> TokenStream {
    let default_member = dispatch.default_member;
    let views = dispatch.members.iter().filter_map(|member| {
        member.view.map(|view| {
            let name = member.name;
            quote! { #view => #name, }
        })
    });
    let identity_rules = dispatch
        .members
        .iter()
        .filter(|member| !member.identity.is_empty())
        .map(|member| {
            let name = member.name;
            let checks = member.identity.iter().map(|(field, expected)| {
                quote! {
                    __identity
                        .and_then(|value| value.get(#field))
                        .and_then(serde_json::Value::as_str)
                        == Some(#expected)
                }
            });
            quote! {
                if #(#checks)&&* {
                    #name
                } else
            }
        });
    let branches = dispatch.members.iter().map(|member| {
        let name = member.name;
        let execution = render_endpoint_execution(member.endpoint, catalog, resource_name);
        quote! { #name => { #execution } }
    });
    let explicit = if dispatch.members.iter().any(|member| member.view.is_some()) {
        quote! {
            if let Some(__view) = __view.as_deref() {
                match __view {
                    #(#views)*
                    _ => unreachable!("clap validates --view"),
                }
            } else
        }
    } else {
        TokenStream::new()
    };
    quote! {
        let __member = #explicit {
            #(#identity_rules)*
            {
                #default_member
            }
        };
        match __member {
            #(#branches,)*
            _ => unreachable!("validated dispatch member"),
        }
    }
}

fn render_request_body_expr(
    endpoint: &Endpoint,
    body: Option<&RequestBody>,
    catalog: &TypeCatalog,
) -> TokenStream {
    match body {
        Some(RequestBody::Flattened {
            type_path,
            args,
            encoding,
        }) => {
            let field_names = args.iter().map(|arg| &arg.pattern);
            let field_values = args.iter().map(|arg| &arg.pattern);
            let encoded = render_typed_body(endpoint, *encoding, catalog);
            quote! {
                let __request_value = #type_path {
                    #(#field_names: #field_values.clone(),)*
                };
                #encoded
            }
        }
        Some(RequestBody::File { encoding }) => {
            let body_ty = render_type_ref(
                endpoint
                    .request_body
                    .as_ref()
                    .expect("RequestBody::File implies a body"),
                catalog,
            );
            let encoded = render_typed_body(endpoint, *encoding, catalog);
            let file_fast_path = match encoding {
                BodyEncoding::Text => Some(quote! {
                    return Ok(Some(crate::client::RequestBody::Text(
                        crate::client::read_request_body_text(__source)?.into_bytes(),
                    )));
                }),
                BodyEncoding::Binary => Some(quote! {
                    return Ok(Some(crate::client::RequestBody::Binary(
                        crate::client::read_request_body_bytes(__source)?,
                    )));
                }),
                _ => None,
            };
            quote! {
                (|| -> Result<Option<crate::client::RequestBody>, crate::error::ClientError> {
                    let __request_json = if let Some(__source) = __body_file.as_deref() {
                        #file_fast_path
                        let __request_text = crate::client::read_request_body_text(__source)?;
                        serde_json::from_str(&__request_text).map_err(|error| {
                            crate::error::ClientError::Decode(format!(
                                "invalid JSON in --body {__source:?}: {error}"
                            ))
                        })?
                    } else if let Some(__source) = __body_json.as_deref() {
                        tokyo_cli_runtime::body::parse_inline_json_request_body(__source)?
                    } else {
                        tokyo_cli_runtime::body::parse_json_request_body_from_field_assignments(__body_fields)?
                    };
                    let __request_value: #body_ty = serde_json::from_value(__request_json)
                        .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
                    Ok(#encoded)
                })()?
            }
        }
        None => quote! { None },
    }
}

fn render_typed_body(
    endpoint: &Endpoint,
    encoding: BodyEncoding,
    catalog: &TypeCatalog,
) -> TokenStream {
    match encoding {
        BodyEncoding::Json => quote! {
            Some(crate::client::RequestBody::Json(
                serde_json::to_vec(&__request_value)
                    .expect("a validated value always serializes"),
            ))
        },
        BodyEncoding::Form => {
            let overrides = endpoint
                .form_field_serializations
                .iter()
                .map(|(name, encoding)| {
                    let style = render_query_style(encoding.serialization);
                    let allow_reserved = encoding.allow_reserved;
                    quote! {
                        crate::client::FormFieldEncoding {
                            name: #name,
                            style: #style,
                            allow_reserved: #allow_reserved,
                        }
                    }
                });
            quote! {
                Some(crate::client::RequestBody::Form(
                    crate::client::build_form_urlencoded_request_body(
                        serde_json::to_value(&__request_value)
                            .expect("a validated value always serializes"),
                        &[#(#overrides),*],
                    )?,
                ))
            }
        }
        BodyEncoding::Multipart => {
            let binary_fields = endpoint
                .request_body
                .as_ref()
                .map(|body| top_level_binary_fields(body, catalog))
                .unwrap_or_default();
            let field_encodings =
                endpoint
                    .multipart_field_encodings
                    .iter()
                    .map(|(name, encoding)| {
                        let content_type = encoding
                            .content_type
                            .as_deref()
                            .map_or_else(|| quote! { None }, |value| quote! { Some(#value) });
                        let headers = encoding.headers.iter().filter_map(|header| {
                            let name = &header.wire_name;
                            constant_string_value(&header.r#type, catalog)
                                .map(|value| quote! { (#name, #value) })
                        });
                        quote! {
                            crate::client::MultipartFieldEncoding {
                                name: #name,
                                content_type: #content_type,
                                headers: &[#(#headers),*],
                            }
                        }
                    });
            quote! {
                Some(crate::client::RequestBody::Multipart(
                    crate::client::build_multipart_request_body(
                        serde_json::to_value(&__request_value)
                            .expect("a validated value always serializes"),
                        &[#(#binary_fields),*],
                        &[#(#field_encodings),*],
                    )?,
                ))
            }
        }
        BodyEncoding::Text => quote! {
            Some(crate::client::RequestBody::Text(
                __request_value.to_string().into_bytes(),
            ))
        },
        BodyEncoding::Binary => quote! {
            Some(crate::client::RequestBody::Binary(
                crate::client::read_request_body_bytes(&__request_value.to_string())?,
            ))
        },
    }
}

fn render_request_target(endpoint: &Endpoint, catalog: &TypeCatalog) -> TokenStream {
    match &endpoint.url_resolution {
        UrlResolution::CallerSuppliedAbsolute { parameter_name } => {
            let field = rust_field_identifier(parameter_name);
            quote! {
                let __absolute_url = #field.to_string();
                let __target = crate::client::RequestTarget::Absolute(&__absolute_url);
            }
        }
        UrlResolution::BaseUrlAndPath => {
            let path = render_path_format(&endpoint.path, &endpoint.path_parameters, catalog);
            match endpoint.server_url.as_deref() {
                Some(server) => quote! {
                    let __path = #path;
                    let __target = crate::client::RequestTarget::ServerAndPath {
                        server: #server,
                        path: &__path,
                    };
                },
                None => quote! {
                    let __path = #path;
                    let __target = crate::client::RequestTarget::Relative(&__path);
                },
            }
        }
    }
}

struct RenderedMedia<'a> {
    request: &'a TokenStream,
    wildcard_error: &'a TokenStream,
}

fn render_request_execution(
    endpoint: &Endpoint,
    catalog: &TypeCatalog,
    resource_name: &str,
    method: &TokenStream,
    auth: &TokenStream,
    accept: &TokenStream,
    media: &RenderedMedia<'_>,
) -> TokenStream {
    if let Some(streaming) = &endpoint.streaming {
        return render_stream_request(
            streaming,
            success_body_ref(endpoint),
            catalog,
            method,
            auth,
            &streaming_response_accept_header(streaming),
            media,
        );
    }

    let buffered = render_buffered_request(
        endpoint,
        catalog,
        resource_name,
        method,
        auth,
        accept,
        media,
    );
    if let Some(conditional) = &endpoint.conditional_streaming {
        let streaming = render_stream_request(
            &conditional.kind,
            Some(&conditional.payload),
            catalog,
            method,
            auth,
            &streaming_response_accept_header(&conditional.kind),
            media,
        );
        let field = &conditional.request_body_field;
        quote! {
            if crate::client::json_request_body_field_is_true(&__request_body, #field) {
                #streaming
            } else {
                #buffered
            }
        }
    } else {
        buffered
    }
}

fn render_buffered_request(
    endpoint: &Endpoint,
    catalog: &TypeCatalog,
    resource_name: &str,
    method: &TokenStream,
    auth: &TokenStream,
    accept: &TokenStream,
    media: &RenderedMedia<'_>,
) -> TokenStream {
    let request_media_type = media.request;
    let wildcard_error_media_type = media.wildcard_error;
    let response_arms = endpoint
        .responses
        .iter()
        .filter(|(status, _)| (200..300).contains(*status))
        .map(|(status, response)| {
            let decode = render_response_decode(
                endpoint,
                response.body.as_ref(),
                response.encoding,
                catalog,
                resource_name,
            );
            quote! { #status => { #decode } }
        });
    quote! {
        let __response = __client.request(
            #method,
            __target,
            &__query,
            &__headers,
            #auth,
            __request_body,
            #accept,
            #request_media_type,
            #wildcard_error_media_type,
        )?;
        match __response.status {
            #(#response_arms,)*
            _ => crate::output::print_untyped_response_body_according_to_content_type(
                &__response.body,
                __response.content_type.as_deref(),
                __output,
            )?,
        }
    }
}

fn render_response_decode(
    endpoint: &Endpoint,
    body_ref: Option<&TypeRef>,
    encoding: ResponseEncoding,
    catalog: &TypeCatalog,
    resource_name: &str,
) -> TokenStream {
    match (encoding, body_ref) {
        (ResponseEncoding::Empty, _) | (_, None) => quote! {},
        (ResponseEncoding::Json, Some(body_ref)) => {
            let ty = render_type_ref(body_ref, catalog);
            let track_created = (endpoint.method == HttpMethod::Post)
                .then(|| identifier_wire_name(body_ref, catalog))
                .flatten()
                .map(|wire_name| {
                    quote! {
                        if let Ok(__created_json) = serde_json::to_value(&__value) {
                            if let Some(__created_id) = __created_json.get(#wire_name) {
                                crate::session::append_created_resource_to_session_reset_log(#resource_name, __created_id);
                            }
                        }
                    }
                });
            quote! {
                let __value: #ty = serde_json::from_slice(&__response.body)
                    .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
                #track_created
                crate::output::print_serialized_response(&__value, __output);
            }
        }
        (ResponseEncoding::Text, Some(_)) => quote! {
            crate::output::print_response_body_as_utf8_text(&__response.body)?;
        },
        (ResponseEncoding::Binary, Some(_)) => quote! {
            crate::output::print_response_body_as_binary_bytes(&__response.body)?;
        },
    }
}

fn render_stream_request(
    streaming: &StreamingKind,
    payload: Option<&TypeRef>,
    catalog: &TypeCatalog,
    method: &TokenStream,
    auth: &TokenStream,
    accept: &str,
    media: &RenderedMedia<'_>,
) -> TokenStream {
    let request_media_type = media.request;
    let wildcard_error_media_type = media.wildcard_error;
    let kind = match streaming {
        StreamingKind::Json => quote! { crate::client::StreamKind::Json },
        StreamingKind::Text => quote! { crate::client::StreamKind::Text },
        StreamingKind::Sse { .. } => quote! { crate::client::StreamKind::Sse },
    };
    let print_item = match streaming {
        StreamingKind::Text => quote! {
            crate::output::print_stream_chunk_as_utf8_text(__payload)?;
        },
        StreamingKind::Sse { .. }
            if payload.is_some_and(|payload| type_ref_is_string_type(payload, catalog)) =>
        {
            quote! {
                let __value = std::str::from_utf8(__payload)
                    .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
                crate::output::print_stream_item_as_json_line(&__value)?;
            }
        }
        StreamingKind::Json | StreamingKind::Sse { .. } => {
            let ty = payload
                .map(|payload| render_type_ref(payload, catalog))
                .unwrap_or_else(|| quote! { serde_json::Value });
            quote! {
                let __value: #ty = serde_json::from_slice(__payload)
                    .map_err(|error| crate::error::ClientError::Decode(error.to_string()))?;
                crate::output::print_stream_item_as_json_line(&__value)?;
            }
        }
    };
    quote! {
        __client.request_stream(
            #method,
            __target,
            &__query,
            &__headers,
            #auth,
            __request_body,
            Some(#accept),
            #request_media_type,
            #wildcard_error_media_type,
            #kind,
            |__payload| {
                #print_item
                Ok(())
            },
        )?;
    }
}

fn streaming_response_accept_header(streaming: &StreamingKind) -> String {
    match streaming {
        StreamingKind::Json => "application/x-ndjson",
        StreamingKind::Text => "text/plain",
        StreamingKind::Sse { .. } => "text/event-stream",
    }
    .to_string()
}

enum RequestBody {
    /// A small, flat, all-primitive object body: rendered as per-field flags
    /// instead of a `--body` file, and assembled back into the declared type.
    Flattened {
        type_path: TokenStream,
        args: Vec<BoundArg>,
        encoding: BodyEncoding,
    },
    /// Anything else: a `--body <FILE>|-` argument. JSON, form, and multipart
    /// inputs use JSON as the CLI-side typed representation; text and binary
    /// inputs are read as raw bytes.
    File { encoding: BodyEncoding },
}

fn render_request_body_input_mode(
    body_ref: &TypeRef,
    encoding: BodyEncoding,
    catalog: &TypeCatalog,
) -> RequestBody {
    match flatten_candidate(body_ref, catalog) {
        Some(object) => RequestBody::Flattened {
            type_path: render_type_ref(body_ref, catalog),
            args: object
                .fields
                .iter()
                .map(|field| {
                    let name = rust_field_identifier(&field.field_name);
                    let ty = render_type_ref(&field.r#type, catalog);
                    let wire_name = &field.wire_name;
                    let default = field.example.as_ref().and_then(example_default_value);
                    let field_attr = match &default {
                        Some(default) => {
                            quote! { #[arg(long = #wire_name, default_value = #default)] }
                        }
                        None => quote! { #[arg(long = #wire_name)] },
                    };
                    let doc = render_doc_attribute_from_description(field.docs.as_deref());
                    BoundArg {
                        field: quote! { #doc #field_attr #name: #ty, },
                        pattern: name,
                        push: TokenStream::new(),
                    }
                })
                .collect(),
            encoding,
        },
        None => RequestBody::File { encoding },
    }
}

pub(crate) fn request_body_mode(
    endpoint: &Endpoint,
    catalog: &TypeCatalog,
) -> Option<&'static str> {
    endpoint.request_body.as_ref().map(|body| {
        if flatten_candidate(body, catalog).is_some() {
            "flattened_flags"
        } else {
            "structured"
        }
    })
}

/// A request body flattens into per-field flags when it's a small, flat,
/// all-primitive object: enough fields to be worth typing individually, few
/// enough not to overwhelm `--help`, and no nested shape that a flag can't
/// represent. The 5-field/primitives-only threshold is a starting point, not
/// a firm rule; it can be tuned against broader fixture coverage. Anything else
/// falls back to `--body`.
fn flatten_candidate<'a>(ty: &TypeRef, catalog: &'a TypeCatalog) -> Option<&'a ObjectType> {
    let TypeRef::Named(id) = ty else { return None };
    let TypeShape::Object(object) = &catalog.declaration(id).shape else {
        return None;
    };
    let flattenable = object.extends.is_empty()
        && !object.extra_properties
        && !object.fields.is_empty()
        && object.fields.len() <= 5
        && object
            .fields
            .iter()
            .all(|field| is_flat_primitive(&field.r#type));
    flattenable.then_some(object)
}

fn top_level_binary_fields(ty: &TypeRef, catalog: &TypeCatalog) -> Vec<String> {
    let object = resolve_type_ref_to_object_type(ty, catalog);
    object
        .map(|object| {
            object
                .fields
                .iter()
                .filter(|field| contains_binary(&field.r#type, catalog))
                .map(|field| field.wire_name.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_type_ref_to_object_type<'a>(
    ty: &TypeRef,
    catalog: &'a TypeCatalog,
) -> Option<&'a ObjectType> {
    match ty {
        TypeRef::Named(id) => match &catalog.declaration(id).shape {
            TypeShape::Object(object) => Some(object),
            TypeShape::Alias { target } => resolve_type_ref_to_object_type(target, catalog),
            _ => None,
        },
        TypeRef::Optional(inner) | TypeRef::Nullable(inner) => {
            resolve_type_ref_to_object_type(inner, catalog)
        }
        _ => None,
    }
}

fn contains_binary(ty: &TypeRef, catalog: &TypeCatalog) -> bool {
    match ty {
        TypeRef::Primitive(PrimitiveType::Binary) => true,
        TypeRef::Named(id) => match &catalog.declaration(id).shape {
            TypeShape::Alias { target } => contains_binary(target, catalog),
            _ => false,
        },
        TypeRef::List(inner) | TypeRef::Optional(inner) | TypeRef::Nullable(inner) => {
            contains_binary(inner, catalog)
        }
        _ => false,
    }
}

fn type_ref_is_string_type(ty: &TypeRef, catalog: &TypeCatalog) -> bool {
    match ty {
        TypeRef::Primitive(PrimitiveType::String) => true,
        TypeRef::Named(id) => match &catalog.declaration(id).shape {
            TypeShape::Alias { target } => type_ref_is_string_type(target, catalog),
            _ => false,
        },
        TypeRef::Optional(inner) | TypeRef::Nullable(inner) => {
            type_ref_is_string_type(inner, catalog)
        }
        _ => false,
    }
}

fn is_flat_primitive(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Primitive(_) => true,
        TypeRef::Optional(inner) => matches!(inner.as_ref(), TypeRef::Primitive(_)),
        _ => false,
    }
}

/// Builds a query-parameter flag and the statement(s) that push its value(s)
/// into the request's `query` pairs — a scalar param pushes at most one pair,
/// a `List`/`Optional<List>` param pushes one pair per element (`explode:
/// true`, OpenAPI's own default and currently the only implemented
/// serialization style).
/// Removes `Nullable` wrappers from a path/query/header parameter's type
/// (recursing through `Optional`/`List`, the only wrappers realistically
/// nested around one) — see `render_param_arg` for why.
fn strip_nullable_type_ref_wrapper(ty: &TypeRef) -> TypeRef {
    match ty {
        TypeRef::Nullable(inner) => strip_nullable_type_ref_wrapper(inner),
        TypeRef::Optional(inner) => {
            TypeRef::Optional(Box::new(strip_nullable_type_ref_wrapper(inner)))
        }
        TypeRef::List(inner) => TypeRef::List(Box::new(strip_nullable_type_ref_wrapper(inner))),
        other => other.clone(),
    }
}

#[derive(Clone, Copy)]
enum JsonArgKind {
    Whole,
    ListItems,
}

fn parameter_json_arg_kind(ty: &TypeRef, catalog: &TypeCatalog) -> Option<JsonArgKind> {
    match ty {
        TypeRef::Optional(inner) | TypeRef::Nullable(inner) => {
            parameter_json_arg_kind(inner, catalog)
        }
        TypeRef::List(inner) if parameter_shape_requires_json(inner, catalog) => {
            Some(JsonArgKind::ListItems)
        }
        ty if parameter_shape_requires_json(ty, catalog) => Some(JsonArgKind::Whole),
        _ => None,
    }
}

fn parameter_shape_requires_json(ty: &TypeRef, catalog: &TypeCatalog) -> bool {
    match ty {
        TypeRef::Primitive(PrimitiveType::Any)
        | TypeRef::Tuple { .. }
        | TypeRef::Map { .. }
        | TypeRef::Intersection(_, _) => true,
        TypeRef::Named(id) => match &catalog.declaration(id).shape {
            TypeShape::Alias { target } => parameter_shape_requires_json(target, catalog),
            TypeShape::Object(_) | TypeShape::Union(_) | TypeShape::UndiscriminatedUnion { .. } => {
                true
            }
            TypeShape::Enum(_) => false,
        },
        TypeRef::Optional(inner) | TypeRef::Nullable(inner) => {
            parameter_shape_requires_json(inner, catalog)
        }
        TypeRef::Primitive(_) | TypeRef::List(_) => false,
    }
}

fn render_parameter_arg_type(ty: &TypeRef, catalog: &TypeCatalog) -> TokenStream {
    match ty {
        TypeRef::Optional(inner) => {
            let inner = render_parameter_arg_type(inner, catalog);
            quote! { Option<#inner> }
        }
        TypeRef::List(inner) if parameter_shape_requires_json(inner, catalog) => {
            quote! { Vec<String> }
        }
        ty if parameter_shape_requires_json(ty, catalog) => quote! { String },
        _ => render_type_ref(ty, catalog),
    }
}

fn render_parameter_value(wire_name: &str, kind: Option<JsonArgKind>) -> TokenStream {
    match kind {
        Some(JsonArgKind::Whole) => quote! {
            crate::client::parse_json_cli_argument(#wire_name, value)?
        },
        Some(JsonArgKind::ListItems) => quote! {
            serde_json::Value::Array(
                value
                    .iter()
                    .map(|item| crate::client::parse_json_cli_argument(#wire_name, item))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        },
        None => quote! {
            serde_json::to_value(value)
                .expect("a validated parameter always serializes")
        },
    }
}

// `__query`/`__headers`, not `query`/`headers`: an OpenAPI parameter can
// itself be named `query` (chrt-fastapi has exactly this — a search-string
// parameter), which would otherwise shadow the accumulator these push into.
fn render_query_arg(param: &Parameter, catalog: &TypeCatalog) -> BoundArg {
    let field = rust_field_identifier(&param.name);
    let param_type = strip_nullable_type_ref_wrapper(&param.r#type);
    let wire_name = &param.wire_name;
    let style = render_query_style(param.serialization);
    let allow_reserved = param.allow_reserved;
    let is_optional = matches!(param_type, TypeRef::Optional(_));
    let json_arg = parameter_json_arg_kind(&param_type, catalog);
    let ty = render_parameter_arg_type(&param_type, catalog);
    let parameter_value = render_parameter_value(wire_name, json_arg);
    let append_value = quote! {
        let __parameter_value = #parameter_value;
        crate::client::append_serialized_parameter_values(
            &mut __query,
            #wire_name,
            &__parameter_value,
            #style,
            #allow_reserved,
        );
    };
    let push = if is_optional {
        quote! {
            if let Some(value) = &#field {
                #append_value
            }
        }
    } else {
        quote! {
            let value = &#field;
            #append_value
        }
    };
    let default = json_arg
        .is_none()
        .then(|| param.example.as_ref().and_then(example_default_value))
        .flatten();
    let field_attr = match default {
        Some(default) => quote! { #[arg(long = #wire_name, default_value = #default)] },
        None if json_arg.is_some() => quote! { #[arg(long = #wire_name, value_name = "JSON")] },
        None => quote! { #[arg(long = #wire_name)] },
    };
    let doc = render_doc_attribute_from_description(param.docs.as_deref());
    BoundArg {
        field: quote! { #doc #field_attr #field: #ty, },
        pattern: field,
        push,
    }
}

fn render_header_arg(param: &Parameter, catalog: &TypeCatalog) -> BoundArg {
    render_header_parameter(param, catalog)
}

fn render_cookie_arg(param: &Parameter, catalog: &TypeCatalog) -> BoundArg {
    let field = rust_field_identifier(&param.name);
    let param_type = strip_nullable_type_ref_wrapper(&param.r#type);
    let ty = render_parameter_arg_type(&param_type, catalog);
    let wire_name = &param.wire_name;
    let style = render_query_style(param.serialization);
    let default = param.example.as_ref().and_then(example_default_value);
    let parameter_value =
        render_parameter_value(wire_name, parameter_json_arg_kind(&param_type, catalog));
    let append = quote! {
        let __parameter_value = #parameter_value;
        let __cookies = crate::client::serialize_cookie_parameter_value(
            #wire_name,
            &__parameter_value,
            #style,
        );
        if !__cookies.is_empty() {
            __headers.push(("Cookie".to_string(), __cookies.join("; ")));
        }
    };
    let push = if matches!(param_type, TypeRef::Optional(_)) {
        quote! {
            if let Some(value) = &#field {
                #append
            }
        }
    } else {
        quote! {
            let value = &#field;
            #append
        }
    };
    let field_attr = match default {
        Some(default) => quote! { #[arg(long = #wire_name, default_value = #default)] },
        None => quote! { #[arg(long = #wire_name)] },
    };
    let doc = render_doc_attribute_from_description(param.docs.as_deref());
    BoundArg {
        field: quote! { #doc #field_attr #field: #ty, },
        pattern: field,
        push,
    }
}

/// Renders an OpenAPI example as a clap `default_value` string literal.
/// Only scalar examples are usable this way — clap parses `default_value`
/// through the same `FromStr` as an explicit flag, so a string/number/bool
/// example works for any scalar-typed arg (`String`, `i64`, `uuid::Uuid`,
/// `chrono::DateTime<Utc>`, ...) with no per-type handling needed. Arrays and
/// objects are skipped: they'd need a different push-shape, not a literal.
fn example_default_value(example: &serde_json::Value) -> Option<String> {
    match example {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn render_header_parameter(param: &Parameter, catalog: &TypeCatalog) -> BoundArg {
    let field = rust_field_identifier(&param.name);
    let param_type = strip_nullable_type_ref_wrapper(&param.r#type);
    let ty = render_parameter_arg_type(&param_type, catalog);
    let wire_name = &param.wire_name;
    let style = render_query_style(param.serialization);
    let default = param.example.as_ref().and_then(example_default_value);
    let parameter_value =
        render_parameter_value(wire_name, parameter_json_arg_kind(&param_type, catalog));
    let append = quote! {
        let __parameter_value = #parameter_value;
        let __parameter_value =
            crate::client::serialize_header_parameter_value(&__parameter_value, #style);
        __headers.push((#wire_name.to_string(), __parameter_value));
    };
    let push = match &param_type {
        TypeRef::Optional(_) => quote! {
            if let Some(value) = &#field {
                #append
            }
        },
        _ => quote! {
            let value = &#field;
            #append
        },
    };
    let field_attr = match &default {
        Some(default) => quote! { #[arg(long = #wire_name, default_value = #default)] },
        None => quote! { #[arg(long = #wire_name)] },
    };
    let doc = render_doc_attribute_from_description(param.docs.as_deref());
    BoundArg {
        field: quote! { #doc #field_attr #field: #ty, },
        pattern: field,
        push,
    }
}

fn render_query_style(serialization: QuerySerialization) -> TokenStream {
    match serialization {
        QuerySerialization::Form { explode: true } => {
            quote! { crate::client::ParameterStyle::FormExplode }
        }
        QuerySerialization::Form { explode: false } => {
            quote! { crate::client::ParameterStyle::Form }
        }
        QuerySerialization::SpaceDelimited => {
            quote! { crate::client::ParameterStyle::SpaceDelimited }
        }
        QuerySerialization::PipeDelimited => {
            quote! { crate::client::ParameterStyle::PipeDelimited }
        }
        QuerySerialization::DeepObject => {
            quote! { crate::client::ParameterStyle::DeepObject }
        }
        QuerySerialization::Simple { explode: true } => {
            quote! { crate::client::ParameterStyle::SimpleExplode }
        }
        QuerySerialization::Simple { explode: false } => {
            quote! { crate::client::ParameterStyle::Simple }
        }
        QuerySerialization::Label { explode: true } => {
            quote! { crate::client::ParameterStyle::LabelExplode }
        }
        QuerySerialization::Label { explode: false } => {
            quote! { crate::client::ParameterStyle::Label }
        }
        QuerySerialization::Matrix { explode: true } => {
            quote! { crate::client::ParameterStyle::MatrixExplode }
        }
        QuerySerialization::Matrix { explode: false } => {
            quote! { crate::client::ParameterStyle::Matrix }
        }
    }
}

/// Builds the block expression that substitutes an endpoint's path parameters
/// (e.g. `/pets/{petId}`) into its path template, percent-encoding each
/// substituted segment. Each segment is bound to its own `let` first rather
/// than inlined into the `format!(...)` call: `prettyplease` can't format
/// inside a macro invocation's arguments (it doesn't know `format!`'s
/// grammar), so anything more than a bare identifier there prints with
/// undifferentiated token spacing — pushing the real expression to a
/// statement keeps the generated file readable.
fn render_path_format(path: &str, params: &[Parameter], catalog: &TypeCatalog) -> TokenStream {
    let mut format_str = String::new();
    let mut bindings = Vec::new();
    let mut args = Vec::new();
    let mut rest = path;
    let mut index: usize = 0;
    while let Some(start) = rest.find('{') {
        format_str.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let end = rest
            .find('}')
            .expect("OpenAPI path templates are well-formed");
        let wire_name = &rest[..end];
        rest = &rest[end + 1..];
        let param = params
            .iter()
            .find(|param| param.wire_name == wire_name)
            .unwrap_or_else(|| panic!("path parameter {wire_name} should be declared"));
        let field = rust_field_identifier(&param.name);
        let segment = format_ident!("__path_segment_{}", index);
        let style = render_query_style(param.serialization);
        let parameter_value = render_parameter_value(
            wire_name,
            parameter_json_arg_kind(&strip_nullable_type_ref_wrapper(&param.r#type), catalog),
        );
        index += 1;
        format_str.push_str("{}");
        bindings.push(quote! {
            let value = &#field;
            let __path_value = #parameter_value;
            let #segment = crate::client::serialize_path_parameter_value(
                #wire_name,
                &__path_value,
                #style,
            );
        });
        args.push(quote! { #segment });
    }
    format_str.push_str(rest);
    quote! {
        {
            #(#bindings)*
            format!(#format_str, #(#args),*)
        }
    }
}

fn render_http_method_token(method: HttpMethod) -> TokenStream {
    match method {
        HttpMethod::Get => quote! { crate::client::Method::Get },
        HttpMethod::Head => quote! { crate::client::Method::Head },
        HttpMethod::Post => quote! { crate::client::Method::Post },
        HttpMethod::Put => quote! { crate::client::Method::Put },
        HttpMethod::Patch => quote! { crate::client::Method::Patch },
        HttpMethod::Delete => quote! { crate::client::Method::Delete },
        HttpMethod::Options => quote! { crate::client::Method::Options },
        HttpMethod::Trace => quote! { crate::client::Method::Trace },
    }
}

/// Preserves OpenAPI's security shape exactly: outer requirements are OR
/// alternatives and every named scheme inside one requirement is ANDed.
fn render_auth_mode(endpoint: &Endpoint) -> TokenStream {
    if endpoint.auth.is_empty() {
        return quote! { crate::client::AuthMode::None };
    }

    let alternatives = endpoint.auth.iter().map(|requirement| {
        let schemes = requirement.schemes.iter().map(|required| {
            let name = &required.scheme.name;
            let scopes = &required.scopes;
            let kind = match &required.scheme.kind {
                AuthSchemeKind::Bearer => quote! { crate::client::AuthSchemeKind::Bearer },
                AuthSchemeKind::Basic => quote! { crate::client::AuthSchemeKind::Basic },
                AuthSchemeKind::Header { name } => {
                    quote! { crate::client::AuthSchemeKind::Header(#name) }
                }
                AuthSchemeKind::QueryKey { name } => {
                    quote! { crate::client::AuthSchemeKind::QueryKey(#name) }
                }
                AuthSchemeKind::CookieKey { name } => {
                    quote! { crate::client::AuthSchemeKind::CookieKey(#name) }
                }
                AuthSchemeKind::OAuth2 {
                    token_endpoint: Some(url),
                } => quote! {
                    crate::client::AuthSchemeKind::OAuth2ClientCredentials(#url)
                },
                AuthSchemeKind::OAuth2 {
                    token_endpoint: None,
                } => quote! { crate::client::AuthSchemeKind::OAuth2Bearer },
                AuthSchemeKind::Inferred { .. } => {
                    quote! { crate::client::AuthSchemeKind::Inferred }
                }
            };
            quote! {
                crate::client::AuthScheme {
                    name: #name,
                    kind: #kind,
                    scopes: &[#(#scopes),*],
                }
            }
        });
        quote! {
            crate::client::AuthAlternative {
                schemes: &[#(#schemes),*],
            }
        }
    });
    quote! {
        crate::client::AuthMode::Requirements(&[
            #(#alternatives),*
        ])
    }
}

/// Translates `x-tokyo-cli-name`/`-hidden`/`-aliases` (already parsed
/// into `CliOverrides` by the importer) into clap `#[command(...)]` attributes
/// on the generated subcommand variant. `-ignore` is handled earlier, by not
/// generating the variant at all.
fn render_cli_overrides(overrides: Option<&CliOverrides>) -> TokenStream {
    let Some(overrides) = overrides else {
        return TokenStream::new();
    };
    let name = overrides.name.as_ref().map(|name| quote! { name = #name, });
    let hidden = overrides.hidden.then(|| quote! { hide = true, });
    let aliases = overrides
        .aliases
        .iter()
        .map(|alias| quote! { alias = #alias, });
    quote! { #[command(#name #hidden #(#aliases)*)] }
}

/// The first declared 2xx response's body type, if any.
fn success_body_ref(endpoint: &Endpoint) -> Option<&TypeRef> {
    endpoint
        .responses
        .iter()
        .find(|(status, _)| (200..300).contains(*status))
        .and_then(|(_, response)| response.body.as_ref())
}

/// Resolves a response body to its declared object shape (through `$ref`/
/// alias indirection — a create response is never itself nullable/optional/a
/// list) and returns the wire name of whichever field OpenAPI's own `id`
/// convention normalized to `field_name == "id"`. That wire name is what
/// actually appears in the JSON: `_id` for the many Mongo-backed APIs that
/// use it, `id` otherwise — either way, this is known at codegen time from
/// the same schema `object_schema_to_declaration` already parsed, so the
/// generated tracking code doesn't have to guess a literal `"id"` key.
fn identifier_wire_name(type_ref: &TypeRef, catalog: &TypeCatalog) -> Option<String> {
    match type_ref {
        TypeRef::Named(id) => match &catalog.declaration(id).shape {
            TypeShape::Object(object) => object
                .fields
                .iter()
                .find(|field| field.field_name == "id")
                .map(|field| field.wire_name.clone()),
            TypeShape::Alias { target } => identifier_wire_name(target, catalog),
            _ => None,
        },
        TypeRef::Nullable(inner) | TypeRef::Optional(inner) => identifier_wire_name(inner, catalog),
        _ => None,
    }
}

fn constant_string_value(type_ref: &TypeRef, catalog: &TypeCatalog) -> Option<String> {
    match type_ref {
        TypeRef::Named(id) => match &catalog.declaration(id).shape {
            TypeShape::Enum(enumeration) if enumeration.values.len() == 1 => {
                Some(enumeration.values[0].wire_value.clone())
            }
            TypeShape::Alias { target } => constant_string_value(target, catalog),
            _ => None,
        },
        TypeRef::Optional(inner) | TypeRef::Nullable(inner) => {
            constant_string_value(inner, catalog)
        }
        _ => None,
    }
}
