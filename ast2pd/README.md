# ast2pd

Convert a JSON AST (from `pd2ast`) back to a PureData `.pd` patch file.

```
ast2pd [OPTIONS] <AST.json | ->

Options:
  -o, --output <FILE>   Write .pd output to FILE instead of stdout
```

## Basic use

```sh
ast2pd my-patch.json
ast2pd my-patch.json -o out.pd
```

## Reading from stdin

Use `-` as the input path to read from stdin, enabling pipeline use:

```sh
ast2pd - < my-patch.json
pd2ast my-patch.pd | ast2pd -
```

## Full roundtrip

Convert a patch to JSON, manipulate it (with `jq` or any other tool), then convert back:

```sh
# Simple roundtrip — output should be semantically identical to input
pd2ast my-patch.pd | ast2pd - -o roundtripped.pd

# Manipulate in the middle — e.g. remove all comment nodes
pd2ast my-patch.pd \
  | jq 'del(.root.nodes[] | select(.kind.kind == "text"))' \
  | ast2pd - -o no-comments.pd
```

The roundtrip preserves the full patch structure: all nodes, connections, sub-patches, GUI objects, arrays, and abstraction bodies (when resolved by `pd2ast`). Position and size information is preserved exactly.

## What changes in the roundtrip

The emitted `.pd` text may differ from the original source in these cosmetic ways — none affect how PureData loads the file:

- Line endings are always CRLF (`\r\n`), regardless of the input.
- Floating-point numbers are re-formatted (e.g. `1e+037` may become the full decimal integer).
- Whitespace within records is normalised to single spaces.
- The order of `#X coords` records relative to `#X connect` records within a canvas is fixed (coords always precede connections in emitted output).
