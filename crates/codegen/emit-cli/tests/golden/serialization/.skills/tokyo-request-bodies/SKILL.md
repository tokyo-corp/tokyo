---
name: tokyo-request-bodies
description: Supplies and validates request bodies for generated Tokyo CLI commands. Use when sending JSON, text, binary, URL-encoded, or multipart input; choosing --body, --body-json, --field, or flattened flags; using stdin; addressing nested fields or arrays; or when the user mentions request body or file upload.
---
# Tokyo Request Bodies

## Inspect the command first

```sh
<cli> schema --command <resource.command>
```

Small flat JSON bodies may become ordinary typed flags:

```sh
<cli> pets create --name Fido --age 4
```

Non-flattened bodies accept exactly one input mode:

```sh
# File, or stdin with -
<cli> orders create --body request.json
printf '%s' '{"reference":"order_1"}' | <cli> orders create --body -

# Inline JSON
<cli> orders create --body-json '{"reference":"order_1"}'

# Repeatable fields
<cli> orders create \
  --field reference=order_1 \
  --field items.0.quantity=2 \
  --field enabled=true
```

`-f` aliases `--field`. Dotted segments address object properties; numeric segments address array indexes. Values that parse as JSON preserve their type (`true`, `2`, `null`, arrays, objects); other values are strings. Quote shell-sensitive JSON.

Duplicate paths and parent/child conflicts such as `a=1` plus `a.b=2` are rejected. JSON request input is assembled and deserialized into the generated type, so unknown shapes or wrong types fail before the request.

## Handle media types

- JSON: all three structured modes are available when the command is not flattened.
- `text/*` or raw binary: `--body FILE`/`--body -` sends source bytes unchanged.
- URL-encoded and multipart: provide the generated JSON input shape.
- For a multipart property with OpenAPI `format: binary`, set its JSON value to a local file path; the CLI reads that file.
- The generic `api METHOD PATH` escape hatch accepts the same body-input modes but has no per-operation generated validation.

For scripts, prefer JSON on stdin with `--body -`, `--output json`, and `--no-input`. Do not put secrets or large payloads directly in argv.
