#!/usr/bin/env python3
"""Writes the output of the tunnel benchmark into the docs.

    benches/scripts/update-docs.py --result result.md --log bench.log \\
        --machine "VM with 4 vCPUs" --rust 1.99.1 --command "..." \\
        v0.5.0 v0.6.2 v0.6.3

--result is what the benchmark printed on stdout, --log what it printed on
stderr. docs/benchmark-results.md gets the setup and the whole table, the
README the machine and the rows of the given versions plus all frp rows.
Only the parts between the bench markers are replaced. Only the Python
standard library is needed.
"""

import argparse
import datetime
import re

RESULTS = "docs/benchmark-results.md"
README = "README.md"


def parse_result(path):
    with open(path, encoding="utf-8") as f:
        lines = f.read().splitlines()
    table = [l for l in lines if l.startswith("|")]
    if len(table) < 3:
        raise SystemExit(f"no result table in {path}")
    # The legend and, if any run stayed busy, the note about the * rows
    legend = [l for l in lines if l.startswith("↑") or l.startswith("\\*")]
    load = next(
        (float(m.group(1)) for l in lines if (m := re.search(r"at most ([\d.]+) % CPU", l))),
        None,
    )
    return table, legend, load


def label(row):
    return row.strip("|").split("|")[0].strip().removesuffix(" *")


def replace(path, name, text):
    with open(path, encoding="utf-8") as f:
        content = f.read()
    start, end = f"<!-- bench:{name} -->\n", f"<!-- /bench:{name} -->"
    pattern = re.compile(re.escape(start) + ".*?" + re.escape(end), re.S)
    if not pattern.search(content):
        raise SystemExit(f"{path} has no {start.strip()} marker")
    content = pattern.sub(lambda _: start + text + end, content)
    with open(path, "w", encoding="utf-8") as f:
        f.write(content)


def joined(items):
    return items[0] if len(items) == 1 else ", ".join(items[:-1]) + " and " + items[-1]


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--result", required=True)
    p.add_argument("--log", required=True)
    p.add_argument("--machine", required=True)
    p.add_argument("--rust", required=True)
    p.add_argument("--command", required=True)
    p.add_argument("--rounds", type=int, default=3)
    p.add_argument("versions", nargs="+", help="rathole versions shown in the README")
    args = p.parse_args()

    table, legend, load = parse_result(args.result)
    header, rows = table[:2], table[2:]
    with open(args.log, encoding="utf-8") as f:
        repeated = f.read().count("machine was busy, repeating")
    frp = sorted(
        {label(r).removeprefix("frp ") for r in rows if label(r).startswith("frp ")},
        key=lambda v: [int(n) for n in re.findall(r"\d+", v)],
    )
    missing = [v for v in args.versions if not any(label(r) == v for r in rows)]
    if missing:
        raise SystemExit(f"not in the result: {', '.join(missing)}")

    if repeated == 0:
        repeats = "No run had to be repeated."
    elif repeated == 1:
        repeats = "One run was repeated because the machine was busy."
    else:
        repeats = f"{repeated} runs were repeated because the machine was busy."
    kept = f" In the kept runs other processes used at most {load:.1f} %." if load is not None else ""
    setup = "\n".join(
        [
            f"- Date: {datetime.date.today().isoformat()}",
            f"- Machine: {args.machine}",
            f"- rathole: every version built from its tag with Rust {args.rust}, release profile and default "
            "features. Tags that no longer build with current Rust get `cargo update -p time` first.",
            f"- frp: {joined(frp)}, the official linux_amd64 releases" if frp else "- frp: not measured",
            f"- Command: `{args.command}`. Every value is the median of {args.rounds} runs.",
            "- Load: before each run the benchmark waits until other processes use less than 10 % of the CPUs, "
            f"and repeats a run if they used more during it. {repeats}{kept}",
        ]
    )
    replace(RESULTS, "setup", setup + "\n")
    replace(RESULTS, "results", "\n".join(table) + "\n\n" + "\n\n".join(legend) + "\n")

    shown = [r for r in rows if label(r) in args.versions or label(r).startswith("frp ")]
    readme = f"Test machine: {args.machine}, Rust {args.rust}.\n\n"
    readme += "\n".join(header + shown) + "\n\n" + "\n\n".join(legend) + "\n"
    replace(README, "readme", readme)


if __name__ == "__main__":
    main()
