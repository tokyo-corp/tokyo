//! Developer-owned route middleware.
//!
//! Tokyo does not overwrite this file after the initial scaffold. Every
//! filesystem route passes through `decorate` before it is registered or run.

use tokyo_cli_runtime::prelude::Route;

/// Adds application-wide middleware to one filesystem route.
pub fn decorate(route: Route) -> Route {
    route
    // Example:
    // .middleware_fn(|context, next| {
    //     eprintln!("running filesystem route");
    //     next.run(context)
    // })
}
