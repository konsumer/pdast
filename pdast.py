"""
pdast - Python wrapper for the pdast CLI tools.

Assumes ast2pd, pd2ast, and pdast2faust are available in PATH.

Functions:
  pd2ast(patch, paths=[], quiet=False, compact=False, include_warnings=False) -> dict
  ast2pd(ast) -> str
  pdast2faust(ast, lib_dirs=[], quiet=False) -> str

All functions accept either a file path (str) or a pre-loaded value:
  - pd2ast:      str path to a .pd file
  - ast2pd:      str path to a JSON file, or a dict (AST)
  - pdast2faust: str path to a JSON file, or a dict (AST)
"""

import json
import subprocess
import shutil


def _require(tool):
    if shutil.which(tool) is None:
        raise FileNotFoundError(
            f"'{tool}' not found on PATH. Install it from https://github.com/konsumer/pdast/releases"
        )


def _run(args, input=None):
    result = subprocess.run(
        args,
        input=input,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip())
    return result.stdout


def pd2ast(patch, paths=[], quiet=False, compact=False, include_warnings=False):
    """
    Convert a PureData .pd patch file to a Python dict (JSON AST).

    Args:
        patch:            Path to the root .pd patch file.
        paths:            Extra directories to search for abstractions.
        quiet:            Suppress warnings.
        compact:          Use minified JSON internally (no effect on return value).
        include_warnings: Include the warnings array in the returned dict.

    Returns:
        dict: The parsed AST.
    """
    _require("pd2ast")
    args = ["pd2ast"]
    for p in paths:
        args += ["--path", p]
    if quiet:
        args.append("--quiet")
    if compact:
        args.append("--compact")
    if include_warnings:
        args.append("--include-warnings")
    args.append(patch)
    return json.loads(_run(args))


def ast2pd(ast):
    """
    Convert a pdast JSON AST to a PureData .pd patch string.

    Args:
        ast: Path to a JSON AST file (str), or a dict containing the AST.

    Returns:
        str: The .pd patch text.
    """
    _require("ast2pd")
    if isinstance(ast, dict):
        return _run(["ast2pd", "-"], input=json.dumps(ast))
    else:
        return _run(["ast2pd", ast])


def pdast2faust(ast, lib_dirs=[], quiet=False):
    """
    Convert a pdast JSON AST to Faust DSP code.

    Args:
        ast:      Path to a JSON AST file (str), or a dict containing the AST.
        lib_dirs: Additional library directories to search for object templates.
        quiet:    Suppress warnings.

    Returns:
        str: The generated Faust DSP source code.
    """
    _require("pdast2faust")
    args = ["pdast2faust"]
    for d in lib_dirs:
        args += ["--lib", d]
    if quiet:
        args.append("--quiet")
    if isinstance(ast, dict):
        return _run(args + ["-"], input=json.dumps(ast))
    else:
        return _run(args + [ast])
