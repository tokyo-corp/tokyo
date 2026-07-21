use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
/// Pagination strategy inferred from an OpenAPI operation.
pub enum Pagination {
    /// Cursor token passed through a request parameter and read from a response field.
    Cursor {
        /// Request parameter carrying the next cursor.
        page_param: String,
        /// Response field containing the next cursor.
        next_field: String,
        /// Response field containing the result array.
        results_field: String,
    },
    /// Offset-based pagination.
    Offset {
        /// Request parameter carrying the offset.
        offset_param: String,
        /// Response field indicating whether another page exists.
        has_next_field: String,
        /// Optional fixed offset increment.
        step: Option<u32>,
    },
    /// Generator-specific custom pagination.
    Custom,
    /// Next-page URL returned in a response field.
    Uri {
        /// Response field containing the next-page URL.
        field: String,
    },
    /// Pagination state is embedded in the request path.
    Path,
}
