//! Route-first runtime primitives for handwritten CLI routes.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;

use crate::client::{
    AuthMode, BufferedResponse, Client, Method, ParameterStyle, QueryParameter, RequestBody,
    RequestTarget, append_serialized_parameter_values,
};
use crate::error::ClientError;
use crate::output::{
    OutputOptions, print_serialized_response, print_untyped_response_body_according_to_content_type,
};

/// The result returned by route handlers and middleware.
pub type RouteResult = Result<RouteResponse, ClientError>;

/// Declares one command-line argument accepted by a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    name: &'static str,
    long: Option<&'static str>,
    short: Option<char>,
    help: Option<&'static str>,
    value_name: Option<&'static str>,
    required: bool,
    multiple: bool,
    flag: bool,
}

impl Argument {
    /// Creates a value-taking argument with the given stable identifier.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            long: None,
            short: None,
            help: None,
            value_name: None,
            required: false,
            multiple: false,
            flag: false,
        }
    }

    /// Uses `--name` for this argument.
    #[must_use]
    pub const fn long(mut self, long: &'static str) -> Self {
        self.long = Some(long);
        self
    }

    /// Uses `-x` for this argument.
    #[must_use]
    pub const fn short(mut self, short: char) -> Self {
        self.short = Some(short);
        self
    }

    /// Sets the help text shown by clap.
    #[must_use]
    pub const fn help(mut self, help: &'static str) -> Self {
        self.help = Some(help);
        self
    }

    /// Sets the displayed value name.
    #[must_use]
    pub const fn value_name(mut self, value_name: &'static str) -> Self {
        self.value_name = Some(value_name);
        self
    }

    /// Requires this argument.
    #[must_use]
    pub const fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Accepts this argument more than once.
    #[must_use]
    pub const fn multiple(mut self) -> Self {
        self.multiple = true;
        self
    }

    /// Makes this argument a boolean switch instead of a value.
    #[must_use]
    pub const fn flag(mut self) -> Self {
        self.flag = true;
        self
    }

    /// Returns the stable argument identifier.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    fn clap_arg(&self) -> clap::Arg {
        let mut argument = clap::Arg::new(self.name)
            .required(self.required)
            .action(if self.flag {
                clap::ArgAction::SetTrue
            } else if self.multiple {
                clap::ArgAction::Append
            } else {
                clap::ArgAction::Set
            });
        if let Some(long) = self.long {
            argument = argument.long(long);
        }
        if let Some(short) = self.short {
            argument = argument.short(short);
        }
        if let Some(help) = self.help {
            argument = argument.help(help);
        }
        if let Some(value_name) = self.value_name {
            argument = argument.value_name(value_name);
        }
        argument
    }
}

/// Parsed route arguments, keyed by their declared identifiers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteArgs {
    values: BTreeMap<String, Vec<String>>,
    flags: BTreeMap<String, bool>,
}

impl RouteArgs {
    /// Creates an empty argument set, useful for direct local-route execution.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            values: BTreeMap::new(),
            flags: BTreeMap::new(),
        }
    }

    /// Builds arguments without invoking clap.
    #[must_use]
    pub fn with_value(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.values
            .entry(name.into())
            .or_default()
            .push(value.into());
        self
    }

    /// Builds a boolean flag without invoking clap.
    #[must_use]
    pub fn with_flag(mut self, name: impl Into<String>, value: bool) -> Self {
        self.flags.insert(name.into(), value);
        self
    }

    /// Returns the last supplied value.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values
            .get(name)
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    /// Returns every supplied value in command-line order.
    #[must_use]
    pub fn get_all(&self, name: &str) -> &[String] {
        self.values.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Returns a required value or a stable decoding error.
    pub fn require(&self, name: &str) -> Result<&str, ClientError> {
        self.get(name)
            .ok_or_else(|| ClientError::Decode(format!("route argument {name:?} was not supplied")))
    }

    /// Parses the last supplied value into a user type.
    pub fn parse<T>(&self, name: &str) -> Result<Option<T>, ClientError>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        self.get(name)
            .map(|value| {
                value.parse::<T>().map_err(|error| {
                    ClientError::Decode(format!(
                        "route argument {name:?} has invalid value {value:?}: {error}"
                    ))
                })
            })
            .transpose()
    }

    /// Parses a required value into a user type.
    pub fn parse_required<T>(&self, name: &str) -> Result<T, ClientError>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        self.parse(name)?
            .ok_or_else(|| ClientError::Decode(format!("route argument {name:?} was not supplied")))
    }

    /// Returns whether a boolean flag was supplied.
    #[must_use]
    pub fn flag(&self, name: &str) -> bool {
        self.flags.get(name).copied().unwrap_or(false)
    }

    fn from_matches(spec: &RouteSpec, matches: &clap::ArgMatches) -> Self {
        let mut parsed = Self::new();
        for argument in &spec.arguments {
            if argument.flag {
                parsed
                    .flags
                    .insert(argument.name.to_string(), matches.get_flag(argument.name));
            } else if let Some(values) = matches.get_many::<String>(argument.name) {
                parsed.values.insert(
                    argument.name.to_string(),
                    values.map(Clone::clone).collect(),
                );
            }
        }
        parsed
    }
}

/// Controls client availability and the default authentication mode for a route.
#[derive(Debug, Clone, Copy, Default)]
pub enum AuthPolicy {
    /// A local or explicitly anonymous route; no client is required.
    #[default]
    None,
    /// A client may be used when supplied.
    Optional(AuthMode),
    /// Route execution requires a client.
    Required(AuthMode),
}

impl AuthPolicy {
    fn auth_mode(self) -> AuthMode {
        match self {
            Self::None => AuthMode::None,
            Self::Optional(mode) | Self::Required(mode) => mode,
        }
    }
}

/// Declarative metadata used to construct and inspect a route.
#[derive(Debug, Clone)]
pub struct RouteSpec {
    name: &'static str,
    about: Option<&'static str>,
    arguments: Vec<Argument>,
    auth: AuthPolicy,
}

impl RouteSpec {
    /// Starts a route declaration.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            about: None,
            arguments: Vec::new(),
            auth: AuthPolicy::None,
        }
    }

    /// Sets the route description.
    #[must_use]
    pub const fn about(mut self, about: &'static str) -> Self {
        self.about = Some(about);
        self
    }

    /// Appends an argument declaration.
    #[must_use]
    pub fn arg(mut self, argument: Argument) -> Self {
        self.arguments.push(argument);
        self
    }

    /// Sets route client/authentication policy.
    #[must_use]
    pub const fn auth(mut self, auth: AuthPolicy) -> Self {
        self.auth = auth;
        self
    }

    /// Returns the command name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the route description shown by clap.
    #[must_use]
    pub const fn description(&self) -> Option<&'static str> {
        self.about
    }

    /// Returns argument declarations in registration order.
    #[must_use]
    pub fn arguments(&self) -> &[Argument] {
        &self.arguments
    }

    /// Returns the route authentication policy.
    #[must_use]
    pub const fn auth_policy(&self) -> AuthPolicy {
        self.auth
    }

    /// Builds the clap command for this route.
    #[must_use]
    pub fn command(&self) -> clap::Command {
        let mut command = clap::Command::new(self.name);
        if let Some(about) = self.about {
            command = command.about(about);
        }
        for argument in &self.arguments {
            command = command.arg(argument.clap_arg());
        }
        command
    }

    /// Parses route-only argv, including the route name as argv[0].
    pub fn parse_from<I, T>(&self, arguments: I) -> Result<RouteArgs, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        self.command()
            .try_get_matches_from(arguments)
            .map(|matches| RouteArgs::from_matches(self, &matches))
    }
}

/// Per-invocation services and parsed values passed through a route chain.
pub struct RouteContext<'a> {
    args: RouteArgs,
    client: Option<&'a Client>,
    output: &'a OutputOptions,
    auth: AuthPolicy,
}

impl<'a> RouteContext<'a> {
    /// Creates a context for direct handler or middleware execution.
    #[must_use]
    pub const fn new(
        args: RouteArgs,
        client: Option<&'a Client>,
        output: &'a OutputOptions,
    ) -> Self {
        Self {
            args,
            client,
            output,
            auth: AuthPolicy::None,
        }
    }

    fn for_route(
        args: RouteArgs,
        client: Option<&'a Client>,
        output: &'a OutputOptions,
        auth: AuthPolicy,
    ) -> Self {
        Self {
            args,
            client,
            output,
            auth,
        }
    }

    /// Returns parsed arguments.
    #[must_use]
    pub const fn args(&self) -> &RouteArgs {
        &self.args
    }

    /// Returns the configured client, if this invocation has one.
    #[must_use]
    pub const fn client_optional(&self) -> Option<&Client> {
        self.client
    }

    /// Returns the configured client or an error suitable for route propagation.
    pub fn client(&self) -> Result<&Client, ClientError> {
        self.client.ok_or(ClientError::MissingConfig(
            "this route requires an HTTP client",
        ))
    }

    /// Returns resolved output options.
    #[must_use]
    pub const fn output(&self) -> &OutputOptions {
        self.output
    }

    /// Starts an HTTP request using this route's client and default auth policy.
    pub fn request<'context>(
        &'context self,
        method: Method,
        target: RequestTarget<'context>,
    ) -> Result<HttpRequestBuilder<'context>, ClientError> {
        Ok(HttpRequestBuilder::new(
            self.client()?,
            method,
            target,
            self.auth.auth_mode(),
        ))
    }
}

/// Builder that delegates request execution to [`Client`].
pub struct HttpRequestBuilder<'a> {
    client: &'a Client,
    method: Method,
    target: RequestTarget<'a>,
    query: Vec<QueryParameter>,
    headers: Vec<(String, String)>,
    auth: AuthMode,
    body: Option<RequestBody>,
    accept: Option<String>,
    request_media_type: Option<String>,
    wildcard_error_media_type: Option<String>,
}

impl<'a> HttpRequestBuilder<'a> {
    fn new(client: &'a Client, method: Method, target: RequestTarget<'a>, auth: AuthMode) -> Self {
        Self {
            client,
            method,
            target,
            query: Vec::new(),
            headers: Vec::new(),
            auth,
            body: None,
            accept: None,
            request_media_type: None,
            wildcard_error_media_type: None,
        }
    }

    /// Adds a form-exploded query value.
    #[must_use]
    pub fn query(mut self, name: &str, value: impl Into<serde_json::Value>) -> Self {
        append_serialized_parameter_values(
            &mut self.query,
            name,
            &value.into(),
            ParameterStyle::FormExplode,
            false,
        );
        self
    }

    /// Adds a query value with explicit OpenAPI serialization.
    #[must_use]
    pub fn serialized_query(
        mut self,
        name: &str,
        value: &serde_json::Value,
        style: ParameterStyle,
        allow_reserved: bool,
    ) -> Self {
        append_serialized_parameter_values(&mut self.query, name, value, style, allow_reserved);
        self
    }

    /// Adds an HTTP header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Overrides the route's default authentication mode.
    #[must_use]
    pub const fn auth(mut self, auth: AuthMode) -> Self {
        self.auth = auth;
        self
    }

    /// Sets a JSON request body.
    pub fn json<T: serde::Serialize>(mut self, value: &T) -> Result<Self, ClientError> {
        self.body = Some(RequestBody::Json(
            serde_json::to_vec(value).map_err(|error| ClientError::Decode(error.to_string()))?,
        ));
        Ok(self)
    }

    /// Sets a UTF-8 request body.
    #[must_use]
    pub fn text(mut self, value: impl Into<String>) -> Self {
        self.body = Some(RequestBody::Text(value.into().into_bytes()));
        self
    }

    /// Sets a binary request body.
    #[must_use]
    pub fn bytes(mut self, value: impl Into<Vec<u8>>) -> Self {
        self.body = Some(RequestBody::Binary(value.into()));
        self
    }

    /// Sets an already serialized request body.
    #[must_use]
    pub fn body(mut self, body: RequestBody) -> Self {
        self.body = Some(body);
        self
    }

    /// Sets the `Accept` header.
    #[must_use]
    pub fn accept(mut self, value: impl Into<String>) -> Self {
        self.accept = Some(value.into());
        self
    }

    /// Sets the request content type.
    #[must_use]
    pub fn content_type(mut self, value: impl Into<String>) -> Self {
        self.request_media_type = Some(value.into());
        self
    }

    /// Supplies a fallback media type for API error bodies.
    #[must_use]
    pub fn error_content_type(mut self, value: impl Into<String>) -> Self {
        self.wildcard_error_media_type = Some(value.into());
        self
    }

    /// Executes through [`Client::request`].
    pub fn send(self) -> Result<BufferedResponse, ClientError> {
        self.client.request(
            self.method,
            self.target,
            &self.query,
            &self.headers,
            self.auth,
            self.body,
            self.accept.as_deref(),
            self.request_media_type.as_deref(),
            self.wildcard_error_media_type.as_deref(),
        )
    }

    /// Executes and converts the buffered response for route output.
    pub fn send_response(self) -> RouteResult {
        self.send().map(RouteResponse::Http)
    }
}

/// A route handler's output, rendered using the existing output runtime.
#[derive(Debug, Clone)]
pub enum RouteResponse {
    /// Produces no output.
    Empty,
    /// A structured value rendered according to [`OutputOptions`].
    Json(serde_json::Value),
    /// UTF-8 text.
    Text(String),
    /// Arbitrary bytes.
    Binary(Vec<u8>),
    /// A buffered HTTP response rendered according to its content type.
    Http(BufferedResponse),
}

impl RouteResponse {
    /// Creates a structured response.
    pub fn json<T: serde::Serialize>(value: T) -> Result<Self, ClientError> {
        serde_json::to_value(value)
            .map(Self::Json)
            .map_err(|error| ClientError::Decode(error.to_string()))
    }

    /// Creates a text response.
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    /// Creates a binary response.
    #[must_use]
    pub fn binary(value: impl Into<Vec<u8>>) -> Self {
        Self::Binary(value.into())
    }

    /// Renders this response using the shared output helpers.
    pub fn render(&self, output: &OutputOptions) -> Result<(), ClientError> {
        match self {
            Self::Empty => Ok(()),
            Self::Json(value) => {
                print_serialized_response(value, output);
                Ok(())
            }
            Self::Text(value) => print_untyped_response_body_according_to_content_type(
                value.as_bytes(),
                Some("text/plain; charset=utf-8"),
                output,
            ),
            Self::Binary(value) => {
                print_untyped_response_body_according_to_content_type(value, None, output)
            }
            Self::Http(response) => print_untyped_response_body_according_to_content_type(
                &response.body,
                response.content_type.as_deref(),
                output,
            ),
        }
    }
}

impl From<BufferedResponse> for RouteResponse {
    fn from(response: BufferedResponse) -> Self {
        Self::Http(response)
    }
}

/// One synchronous middleware stage.
pub trait Middleware: Send + Sync {
    /// Handles this stage and may invoke, wrap, or skip the remaining chain.
    fn handle(&self, context: &mut RouteContext<'_>, next: MiddlewareNext<'_>) -> RouteResult;
}

struct FunctionMiddleware<F>(F);

impl<F> Middleware for FunctionMiddleware<F>
where
    F: for<'context, 'next> Fn(&mut RouteContext<'context>, MiddlewareNext<'next>) -> RouteResult
        + Send
        + Sync,
{
    fn handle(&self, context: &mut RouteContext<'_>, next: MiddlewareNext<'_>) -> RouteResult {
        (self.0)(context, next)
    }
}

type Handler =
    dyn for<'context> Fn(&mut RouteContext<'context>) -> RouteResult + Send + Sync + 'static;

/// The unexecuted remainder of a middleware chain.
pub struct MiddlewareNext<'a> {
    middleware: &'a [Arc<dyn Middleware>],
    handler: &'a Handler,
}

impl<'a> MiddlewareNext<'a> {
    /// Runs the next registered middleware, or the route handler at the end.
    pub fn run(self, context: &mut RouteContext<'_>) -> RouteResult {
        if let Some((middleware, remaining)) = self.middleware.split_first() {
            middleware.handle(
                context,
                Self {
                    middleware: remaining,
                    handler: self.handler,
                },
            )
        } else {
            (self.handler)(context)
        }
    }
}

/// A registered route with its handler and middleware.
pub struct Route {
    spec: RouteSpec,
    handler: Arc<Handler>,
    middleware: Vec<Arc<dyn Middleware>>,
}

impl Route {
    /// Creates a route from a spec and handler.
    #[must_use]
    pub fn new<F>(spec: RouteSpec, handler: F) -> Self
    where
        F: for<'context> Fn(&mut RouteContext<'context>) -> RouteResult + Send + Sync + 'static,
    {
        Self {
            spec,
            handler: Arc::new(handler),
            middleware: Vec::new(),
        }
    }

    /// Appends middleware. Execution enters middleware in registration order
    /// and unwinds in reverse order when each stage calls `next.run`.
    #[must_use]
    pub fn middleware<M>(mut self, middleware: M) -> Self
    where
        M: Middleware + 'static,
    {
        self.middleware.push(Arc::new(middleware));
        self
    }

    /// Appends middleware expressed as a closure.
    #[must_use]
    pub fn middleware_fn<F>(self, middleware: F) -> Self
    where
        F: for<'context, 'next> Fn(
                &mut RouteContext<'context>,
                MiddlewareNext<'next>,
            ) -> RouteResult
            + Send
            + Sync
            + 'static,
    {
        self.middleware(FunctionMiddleware(middleware))
    }

    /// Returns declarative route metadata.
    #[must_use]
    pub const fn spec(&self) -> &RouteSpec {
        &self.spec
    }

    /// Executes a route from already parsed arguments.
    pub fn run(
        &self,
        args: RouteArgs,
        client: Option<&Client>,
        output: &OutputOptions,
    ) -> RouteResult {
        if matches!(self.spec.auth, AuthPolicy::Required(_)) && client.is_none() {
            return Err(ClientError::MissingConfig(
                "this route requires an HTTP client",
            ));
        }
        let mut context = RouteContext::for_route(args, client, output, self.spec.auth);
        MiddlewareNext {
            middleware: &self.middleware,
            handler: self.handler.as_ref(),
        }
        .run(&mut context)
    }

    /// Parses route argv and executes the route.
    pub fn run_from<I, T>(
        &self,
        arguments: I,
        client: Option<&Client>,
        output: &OutputOptions,
    ) -> Result<RouteResponse, RouteRunError>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let args = self.spec.parse_from(arguments)?;
        self.run(args, client, output)
            .map_err(RouteRunError::Client)
    }

    /// Executes a route from matches produced by a larger clap command tree.
    pub fn run_matches(
        &self,
        matches: &clap::ArgMatches,
        client: Option<&Client>,
        output: &OutputOptions,
    ) -> RouteResult {
        self.run(RouteArgs::from_matches(&self.spec, matches), client, output)
    }
}

/// Parse or runtime failure from [`Route::run_from`].
#[derive(Debug)]
pub enum RouteRunError {
    /// clap rejected the command line.
    Parse(clap::Error),
    /// The handler, middleware, or HTTP client failed.
    Client(ClientError),
}

impl From<clap::Error> for RouteRunError {
    fn from(error: clap::Error) -> Self {
        Self::Parse(error)
    }
}

impl std::fmt::Display for RouteRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(formatter),
            Self::Client(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RouteRunError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::OutputFormat;
    use std::io::{Read as _, Write as _};
    use std::sync::Mutex;

    fn output() -> OutputOptions {
        OutputOptions {
            format: OutputFormat::JsonRaw,
            fields: None,
        }
    }

    #[test]
    fn arguments_parse_values_repetitions_flags_and_types() {
        let spec = RouteSpec::new("inspect")
            .arg(Argument::new("id").required())
            .arg(Argument::new("tag").long("tag").multiple())
            .arg(Argument::new("verbose").long("verbose").short('v').flag());
        let args = spec
            .parse_from(["inspect", "42", "--tag", "a", "--tag", "b", "-v"])
            .expect("arguments parse");

        assert_eq!(args.require("id").unwrap(), "42");
        assert_eq!(args.parse_required::<u16>("id").unwrap(), 42);
        assert_eq!(args.get_all("tag"), ["a", "b"]);
        assert!(args.flag("verbose"));
        assert_eq!(args.get("missing"), None);
    }

    #[test]
    fn argument_access_reports_missing_and_invalid_values() {
        let args = RouteArgs::new().with_value("count", "many");
        assert!(
            args.require("missing")
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        assert!(
            args.parse_required::<usize>("count")
                .unwrap_err()
                .to_string()
                .contains("many")
        );
    }

    #[test]
    fn route_spec_preserves_builder_order_and_auth_policy() {
        let spec = RouteSpec::new("show")
            .about("Show a thing")
            .arg(Argument::new("first"))
            .arg(Argument::new("second").long("second"))
            .auth(AuthPolicy::Required(AuthMode::None));

        assert_eq!(spec.name(), "show");
        assert_eq!(
            spec.arguments()
                .iter()
                .map(Argument::name)
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(matches!(
            spec.auth_policy(),
            AuthPolicy::Required(AuthMode::None)
        ));
        spec.command().debug_assert();
    }

    struct RecordingMiddleware {
        name: &'static str,
        events: Arc<Mutex<Vec<String>>>,
        short_circuit: bool,
    }

    impl Middleware for RecordingMiddleware {
        fn handle(&self, context: &mut RouteContext<'_>, next: MiddlewareNext<'_>) -> RouteResult {
            self.events
                .lock()
                .unwrap()
                .push(format!("{}:before", self.name));
            if self.short_circuit {
                return RouteResponse::json("stopped");
            }
            let response = next.run(context);
            self.events
                .lock()
                .unwrap()
                .push(format!("{}:after", self.name));
            response
        }
    }

    #[test]
    fn middleware_order_is_deterministic_and_onion_shaped() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let handler_events = events.clone();
        let route = Route::new(RouteSpec::new("local"), move |_| {
            handler_events.lock().unwrap().push("handler".to_string());
            Ok(RouteResponse::Empty)
        })
        .middleware(RecordingMiddleware {
            name: "first",
            events: events.clone(),
            short_circuit: false,
        })
        .middleware(RecordingMiddleware {
            name: "second",
            events: events.clone(),
            short_circuit: false,
        });

        route.run(RouteArgs::new(), None, &output()).unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            [
                "first:before",
                "second:before",
                "handler",
                "second:after",
                "first:after"
            ]
        );
    }

    #[test]
    fn middleware_can_short_circuit_the_remainder() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let route = Route::new(RouteSpec::new("local"), |_| Ok(RouteResponse::Empty))
            .middleware(RecordingMiddleware {
                name: "stop",
                events: events.clone(),
                short_circuit: true,
            })
            .middleware(RecordingMiddleware {
                name: "unreached",
                events: events.clone(),
                short_circuit: false,
            });

        route.run(RouteArgs::new(), None, &output()).unwrap();
        assert_eq!(*events.lock().unwrap(), ["stop:before"]);
    }

    #[test]
    fn local_route_runs_without_constructing_a_client() {
        let route = Route::new(RouteSpec::new("version"), |context| {
            assert!(context.client_optional().is_none());
            RouteResponse::json(serde_json::json!({"version": 1}))
        });
        let response = route.run(RouteArgs::new(), None, &output()).unwrap();
        assert!(matches!(response, RouteResponse::Json(_)));
    }

    #[test]
    fn client_required_route_fails_before_middleware_or_handler() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let handler_events = events.clone();
        let route = Route::new(
            RouteSpec::new("remote").auth(AuthPolicy::Required(AuthMode::None)),
            move |_| {
                handler_events.lock().unwrap().push("handler".to_string());
                Ok(RouteResponse::Empty)
            },
        )
        .middleware(RecordingMiddleware {
            name: "middleware",
            events: events.clone(),
            short_circuit: false,
        });

        let error = route
            .run(RouteArgs::new(), None, &output())
            .expect_err("required client should be checked");
        assert!(matches!(error, ClientError::MissingConfig(_)));
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn http_builder_delegates_headers_query_and_body_to_client() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 1024];
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some(headers_end) = text.find("\r\n\r\n") {
                    let content_length = text[..headers_end]
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|value| value.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= headers_end + 4 + content_length {
                        break;
                    }
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}",
                )
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        let client = Client::new(
            format!("http://{address}"),
            None,
            None,
            None,
            BTreeMap::new(),
            "test".to_string(),
            crate::profile::default_credential_store(),
            false,
        );
        let route = Route::new(
            RouteSpec::new("create").auth(AuthPolicy::Required(AuthMode::None)),
            |context| {
                context
                    .request(Method::Post, RequestTarget::Relative("/items"))?
                    .query("page", 2)
                    .header("X-Route", "custom")
                    .json(&serde_json::json!({"name": "Tokyo"}))?
                    .send_response()
            },
        );

        let response = route
            .run(RouteArgs::new(), Some(&client), &output())
            .unwrap();
        let request = server.join().unwrap();
        assert!(matches!(response, RouteResponse::Http(response) if response.status == 200));
        assert!(request.starts_with("POST /items?page=2 HTTP/1.1\r\n"));
        assert!(request.to_ascii_lowercase().contains("x-route: custom\r\n"));
        assert!(request.ends_with("{\"name\":\"Tokyo\"}"));
    }
}
