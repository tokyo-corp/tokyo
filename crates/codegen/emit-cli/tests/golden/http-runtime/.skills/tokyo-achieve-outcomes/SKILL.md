---
name: tokyo-achieve-outcomes
description: Executes goal-oriented generated Tokyo CLI outcomes with achieve. Use when creating resources, repeating creations, supplying natural-language intent, overriding inferred fields, dry-running a generated plan, finalizing submissions, or when the user mentions achieve, --count, --prompt, --set, --submit, or created IDs.
---
# Tokyo Achieve Outcomes

## Prefer outcomes over low-level calls

Inspect advertised outcomes:

```sh
<cli> --output json --no-input start
<cli> achieve --help
```

Create resources:

```sh
<cli> --output json --no-input achieve create pet --count 3
```

Inspect without executing:

```sh
<cli> --output json --no-input achieve create pet --dry-run
```

Override inferred body fields with repeatable dotted assignments:

```sh
<cli> --output json --no-input achieve create pet \
  --set name=Fido --set metadata.active=true
```

Builder-backed outcomes may accept natural-language intent and finalization:

```sh
<cli> --output json --no-input achieve create report \
  --prompt "Summarize Q2 revenue by region" --submit
```

## Know the inference boundary

`achieve` infers create operations from POST operation names/summaries using create-like verbs, and finalize outcomes from POST/PATCH verbs such as submit, stage, publish, finalize, activate, or approve. It prefers a create operation with a populated request schema.

It synthesizes constants, defaults, examples, enum values, required fields, and safe primitive placeholders. It may resolve `*_id` relationships from allowlisted caller identity or cheap caller-reachable GET list operations. `--set` overlays the inferred body. `--prompt` is valid only when the schema contains `prompt`, `text`, `instructions`, or `message`.

Create count must be `1..=1000`. Non-create outcomes require one or more `--id`. `--submit` is valid only for create and runs the inferred finalize operation after successful creation. Validation responses with `validation_passed: false` fail the outcome. Streaming body toggles are forced off because `achieve` buffers nested command results.

The final JSON includes the goal, count, selected operation, created identifiers, submitted results, execution status, and raw results. If inference cannot safely produce a value, supply the requested `--set PATH=VALUE` or use the direct command from `schema`.
