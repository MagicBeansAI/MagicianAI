"""Read `magician-config.yaml` complete, with its router tables spliced in.

The router's `profiles` and `operation_mapping` live in a sibling
`llm-router.yaml` rather than in the config itself — together they were roughly
4,000 of the config's 6,200 lines, and they are the only parts edited at
anything like that cadence.

That makes reading the config file directly a bug: the text on its own has no
profiles and no operation mapping. Scripts that parsed it for routing silently
saw an empty table, which is how five eval tests started reporting
"no pre-plan operation mappings found" against a config that has 235 lines of
them.

This mirrors `magician::config::splice_router_tables_into` deliberately, rather
than reimplementing a YAML merge. The tables alias `*local_generation_model`,
whose anchor is defined in `runtime:` of the main config, and YAML aliases do
not cross documents — so the join has to happen on the text, before any parse,
exactly as the Rust loader does it.

Use `read_config_text(path)` wherever a script used `path.read_text()`, and pass
the result to `yaml.safe_load` or a line parser as before.
"""

from __future__ import annotations

from pathlib import Path

ROUTER_TABLES_FILE = "llm-router.yaml"


def config_has_inline_router_tables(config_text: str) -> bool:
    """Whether the text already carries a router table itself.

    Either `profiles` or `operation_mapping` counts. The two always move
    together in a real config, and accepting either lets a synthetic fixture
    that exercises only one of them parse without inventing a sibling file.

    Checked on the text rather than after parsing, because this decides what to
    parse — and because an absent `profiles` key deserialises to an empty table
    rather than an error, so after parsing "absent" and "empty" look identical.
    """
    in_llm = False
    in_router = False
    for line in config_text.splitlines():
        # Comments carry no indentation contract — `llm-router.yaml` opens with
        # a column-0 header block, and treating that as a dedent would end the
        # `llm:` section before its own tables were seen.
        if line.lstrip().startswith("#"):
            continue
        if line.startswith("llm:"):
            in_llm = True
            continue
        if in_llm and line[:1] not in (" ", "") and line.strip():
            in_llm = False
            in_router = False
            continue
        if not in_llm:
            continue
        if line.startswith("  ") and not line.startswith("   ") and line.strip():
            in_router = line.startswith("  router:")
            continue
        if in_router and (
            line.startswith("    profiles:") or line.startswith("    operation_mapping:")
        ):
            return True
    return False


def splice_router_tables_into(config_text: str, tables_text: str) -> str:
    """Insert the tables under `llm.router`, keeping it a single document."""
    out: list[str] = []
    in_llm = False
    spliced = False
    for line in config_text.splitlines():
        out.append(line)
        if line.startswith("llm:"):
            in_llm = True
        elif in_llm and line[:1] not in (" ", "") and line.strip():
            in_llm = False
        if in_llm and not spliced and line.startswith("  router:"):
            out.append(_tables_body(tables_text))
            spliced = True
    if not spliced:
        raise ValueError("config text has no `llm.router:` key to splice router tables into")
    return "\n".join(out) + "\n"


def read_config_text(config_path: str | Path) -> str:
    """The complete config document, tables included.

    The tables are resolved as a sibling of the config actually being read,
    never through an independent search order, so a runtime-root config can
    never pair with the repository's tables.
    """
    config_path = Path(config_path)
    config_text = config_path.read_text(encoding="utf-8")
    if config_has_inline_router_tables(config_text):
        return config_text
    tables_path = config_path.parent / ROUTER_TABLES_FILE
    if not tables_path.exists():
        raise FileNotFoundError(
            f"{tables_path} is missing; {config_path} does not carry its router "
            "tables inline and cannot be read without them"
        )
    return splice_router_tables_into(
        config_text, tables_path.read_text(encoding="utf-8")
    )


def _tables_body(tables_text: str) -> str:
    """The tables without their column-0 file header.

    The header is a comment block explaining the file, which is valid YAML
    anywhere — `serde_yaml` on the Rust side ignores it wherever it lands. But
    several scripts parse this config by indentation rather than with a YAML
    library, and a column-0 line appearing inside a nested mapping reads to them
    as a dedent out of `llm.router`, which silently emptied the very tables they
    were looking for. Dropping the leading comment block costs nothing: comments
    that document individual entries are indented with them and are preserved.
    """
    lines = tables_text.splitlines()
    start = 0
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped and not stripped.startswith("#"):
            start = index
            break
    return "\n".join(lines[start:]).rstrip("\n")


if __name__ == "__main__":
    # CLI so shell and Ruby consumers get the same complete document without a
    # third and fourth implementation of the splice:
    #   python3 scripts/magician_config_text.py <config> > spliced.yaml
    import sys as _sys

    if len(_sys.argv) != 2:
        print("usage: magician_config_text.py <magician-config.yaml>", file=_sys.stderr)
        raise SystemExit(2)
    _sys.stdout.write(read_config_text(_sys.argv[1]))
