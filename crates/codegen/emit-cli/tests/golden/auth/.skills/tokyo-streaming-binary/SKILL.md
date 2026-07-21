---
name: tokyo-streaming-binary
description: Handles streaming and binary input or output from generated Tokyo CLIs. Use when consuming SSE, NDJSON, or text streams; setting a stream selector; downloading raw bytes; uploading binary files; redirecting output; or when the user mentions streaming, event-stream, NDJSON, binary, octet-stream, or PDF.
---
# Tokyo Streaming and Binary Data

## Consume streams incrementally

Generated streaming commands do not buffer the full response:

```sh
<cli> events watch --output json --no-input |
  while IFS= read -r item; do
    printf '%s\n' "$item"
  done
```

- SSE and NDJSON payloads are decoded and emitted as one compact JSON value per line.
- Text streams write UTF-8 chunks directly.
- Consume stdout line-by-line; do not wait for one JSON array or pretty-printed document.
- Diagnostics and structured errors remain on stderr.

Some operations support both buffered JSON and streaming based on a boolean request-body field, commonly `stream`:

```sh
<cli> chat create --message "hello" --stream true
```

Use `schema --command <ID>` to verify the actual field/flag. Tokyo only treats mixed JSON/stream responses as conditional streaming when the request schema has the required boolean selector.

## Preserve binary bytes

Redirect binary responses:

```sh
<cli> files download FILE_ID > artifact.bin
```

Binary response bodies are written byte-for-byte to stdout. Do not request JSON output, parse stdout as text, or mix progress messages into stdout. Validate the process exit code before trusting the file.

Send a raw binary request from a file:

```sh
<cli> uploads put URL --body ./artifact.bin
```

For multipart requests, a property declared with `format: binary` takes a local file path in the JSON-shaped body.

## Generation constraints

Tokyo supports JSON, `text/*`, and binary application responses. Standard `text/event-stream` and NDJSON media types can infer streaming; explicit streaming metadata can describe text, JSON, or SSE. Pagination and streaming cannot be combined. Resumable SSE/`Last-Event-ID` reconnect semantics are not implemented, so callers must implement any safe restart policy externally.
