#!/usr/bin/env python3
"""Independently check and graph the generated contract-reference coverage."""

from __future__ import annotations

import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import BLACK, BLUE, GRAY, GREEN, RED, Canvas  # noqa: E402


def plugin_block(page: str, plugin: str) -> str:
    marker = f"## `{plugin}`\n"
    start = page.find(marker)
    if start < 0:
        raise ValueError(f"missing plugin heading: {plugin}")
    end = page.find("\n## `", start + len(marker))
    return page[start:] if end < 0 else page[start:end]


def table_value(block: str, label: str) -> str:
    match = re.search(rf"^\| {re.escape(label)} \| (.*?) \|$", block, re.M)
    if not match:
        raise ValueError(f"missing {label!r} contract row")
    return match.group(1)


def check_contract(page: str, expected: dict) -> list[bool]:
    block = plugin_block(page, expected["plugin"])
    source = expected["source"]
    source_file = ROOT / source
    if not source_file.is_file():
        raise ValueError(f"fixture source does not exist: {source}")
    source_ok = f"[`{source}`](" in block
    config_ok = f"### `[{expected['section']}]` configuration" in block
    for field in expected["fields"]:
        row = re.search(rf"^\| `{re.escape(field)}` \| .*?\| \[`([^`]+)`\]\([^)]*#L(\d+)\) \|$", block, re.M)
        if not row:
            raise ValueError(f"missing precise source link for {expected['plugin']}.{field}")
        linked_source, line = row.groups()
        if not linked_source.startswith(f"{source}:"):
            raise ValueError(f"wrong source for {field}: {linked_source}")
        source_lines = source_file.read_text().splitlines()
        line_number = int(line)
        source_span = source_lines[line_number - 1 : line_number + 4]
        if not any(field in source_line for source_line in source_span):
            raise ValueError(f"source link does not cover {field}: {linked_source}")
    return [
        source_ok,
        config_ok,
        table_value(block, "Schedule participation") == expected["schedule"],
        table_value(block, "Public extension points") == expected["extension"],
        table_value(block, "Exchange ports / wire types") == expected["exchange"],
    ]


def main() -> None:
    result = subprocess.run(
        ["cargo", "run", "--quiet", "--example", "contract_reference"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=True,
    )
    expected = json.loads((HERE / "data" / "expected_contracts.json").read_text())
    rows = []
    for contract in expected:
        try:
            checks = check_contract(result.stdout, contract)
        except ValueError as error:
            print(f"FAIL: {error}")
            raise SystemExit(1) from error
        rows.append(checks)
    if not all(all(row) for row in rows):
        raise SystemExit("FAIL: generated contract reference differs from independent fixture")

    out = HERE / "plots"
    out.mkdir(exist_ok=True)
    canvas = Canvas(900, 420)
    canvas.text(28, 24, "GENERATED CONTRACT COVERAGE", BLACK, 3)
    canvas.text(28, 52, "live generator observed against independent fixture and source links", BLUE, 2)
    columns = ["SOURCE", "CONFIG", "SCHEDULE", "EXTENSION", "EXCHANGE"]
    x0, y0, cell_w, cell_h = 245, 105, 122, 42
    for column, label in enumerate(columns):
        canvas.text(x0 + column * cell_w + 16, 80, label, GRAY, 1)
    for row, (contract, checks) in enumerate(zip(expected, rows)):
        y = y0 + row * cell_h
        canvas.text(28, y + 13, f"PLUGIN {row + 1}", BLACK, 2)
        for column, passed in enumerate(checks):
            x = x0 + column * cell_w
            color = GREEN if passed else RED
            canvas.rect(x, y, x + cell_w - 8, y + cell_h - 8, color)
            canvas.text(x + 34, y + 10, "MATCH" if passed else "MISS", BLACK, 1)
    count = len(rows) * len(columns)
    canvas.text(28, 350, f"PASS: {count}/{count} required generated facts match fixture", BLACK, 2)
    canvas.text(28, 376, "fixture also verifies every config-field link covers its Rust declaration", GRAY, 1)
    canvas.save(out / "contract_reference_coverage.png")
    print(f"PASS=True checks={count}/{count} plugins={len(rows)}")


if __name__ == "__main__":
    main()
