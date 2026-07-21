---
name: tokyo-filesystem-routes
description: Creates and maintains filesystem routes in generated Tokyo CLI projects. Use when adding a command, nesting command groups, editing src/routes, defining Route or RouteSpec, adding route arguments, using RouteResponse, or applying route middleware.
---
# Tokyo Filesystem Routes

## Create a command

The path below `src/routes` defines the command path:

```text
src/routes/index.rs          -> <cli> index
src/routes/hello.rs          -> <cli> hello
src/routes/users/list_all.rs -> <cli> users list-all
```

`mod.rs` organizes Rust modules but does not create a command. Use valid Rust identifiers in filenames; underscores become hyphens. Avoid names that collide with generated OpenAPI resources or built-ins such as `start`, `schema`, and `auth`.

Every route file exports `pub fn route() -> Route`:

```rust
use tokyo_cli_runtime::prelude::*;

pub fn route() -> Route {
    Route::new(
        RouteSpec::new("hello")
            .about("Greet somebody")
            .arg(Argument::new("name").required()),
        |context| {
            let name = context.args().require("name")?;
            Ok(RouteResponse::text(format!("Hello, {name}!")))
        },
    )
}
```

The file path, not the `RouteSpec` name, determines the clap command path.

## Choose a response

Return `RouteResponse::text`, JSON, binary bytes, or a buffered HTTP response as appropriate. Local routes require no API connection. For API-backed routes, obtain the optional client from `RouteContext` and construct requests with `HttpRequestBuilder`.

## Apply shared middleware

`src/middleware.rs` decorates every filesystem route before help, metadata, or dispatch:

```rust
pub fn decorate(route: Route) -> Route {
    route.middleware_fn(|context, next| {
        eprintln!("running filesystem route");
        next.run(context)
    })
}
```

Middleware runs in registration order and may inspect context, wrap the next stage, transform the response, or return early. It does not decorate OpenAPI-generated commands.

## Register changes

Run `tokyo generate` after route file changes, or keep `tokyo dev` running to regenerate and type-check automatically.
