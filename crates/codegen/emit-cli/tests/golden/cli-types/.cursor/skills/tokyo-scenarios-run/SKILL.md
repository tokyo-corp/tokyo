---
name: tokyo-scenarios-run
description: Authors, discovers, and executes Tokyo scenario recipes with structured output. Use when building multi-step CLI workflows, running scenario files, repeating commands, passing --set values, capturing @last responses, collecting IDs, featuring next steps, or when the user mentions run, .scenario, @repeat, @let, @collect, or @output.
---
# Tokyo Scenarios

## Discover and run

```sh
<cli> run list
<cli> --output json --no-input run seed-data --set count=3
<cli> --output json --no-input run ./smoke.scenario
```

Scenario files are discovered by filename from:

1. directories in `<ENV_PREFIX>_SCENARIO_PATH`
2. the CLI user config `scenarios` directory
3. `.<command>/scenarios` under the current directory

Later locations replace same-named earlier scenarios. An explicit file path also works. Embedded scenarios can be configured in `tokyo.toml`.

## Author a recipe

```text
# description: Create several records
# allowed-environments: Development, Local
# usage: run seed-data --set count=3
# featured: true
@repeat {{count}}
items create --field name=item-{{i}}
@let item_id=@last:/id
@collect item_ids=@var:item_id
@end
@output requested=@set:count
```

- `--set KEY=VALUE` supplies `@set:KEY` and `{{KEY}}`.
- `@last:/pointer` reads a JSON Pointer from the latest command response.
- `@let NAME=VALUE` preserves a value after later commands replace `@last:`.
- `@collect NAME=VALUE` appends to an output array.
- `@output NAME=VALUE` sets a final output field.
- Inside repeats, `{{i}}` is zero-based and `{{index}}` is one-based.
- Repeat count must be a non-negative integer no greater than 10,000.
- `--repeat` and `# repeat:` alias `@repeat`; `--end`, `# end`, and `# endrepeat` can close it.

A matching `--set NAME=VALUE` overrides an `@let NAME=...` binding. Auth providers may expose allowlisted `@self:FIELD` values.

## Execution invariants

Commands run in order and execution stops at the first failure with its scenario line number. Normal nested output is captured. If any `@collect` or `@output` fields exist, stdout receives one final JSON object; otherwise it receives the last command response or null. Use `# allowed-environments:` to prevent a recipe from running against unintended named environments. Mark only high-value, safe recipes `# featured: true`.
