# pd2ast

Convert a `.pd` file to a JSON AST.

```
pd2ast [OPTIONS] <PATCH.pd>

Options:
  -p, --path <DIR>      Extra search path for abstractions (repeatable)
  -o, --output <FILE>   Write JSON to FILE instead of stdout
  -q, --quiet           Suppress warnings
      --compact         Minified JSON output
      --include-warnings  Include the warnings array in the JSON output
```

## Basic use

```sh
pd2ast my-patch.pd
pd2ast my-patch.pd > my-patch.json
pd2ast my-patch.pd -o my-patch.json
```

## Abstractions

pd2ast resolves abstractions (external `.pd` files referenced by name) the same way PureData does: it searches the patch's own directory first, then any extra `-p` paths.

```sh
pd2ast my-patch.pd -p ~/pd-externals -p ~/pd-abstractions
```

If an abstraction cannot be found, the object is stored as `unknown` in the AST and a warning is printed to stderr. Use `--quiet` to suppress warnings.

## Compact output

```sh
pd2ast my-patch.pd --compact
```

## Including warnings in the output

```sh
pd2ast my-patch.pd --include-warnings
```

## Pipeline use

```sh
pd2ast my-patch.pd | jq '.root.nodes[] | select(.kind.kind == "obj")'
```
