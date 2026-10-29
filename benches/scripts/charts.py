#!/usr/bin/env python3
"""Draws bar charts from the result table of the tunnel benchmark.

    benches/scripts/charts.py docs/benchmark-results.md docs/img v0.6.1 v0.6.2 v0.6.3 "frp v0.71.1"

Reads the first markdown table with a "Transport" column from the given file
and writes one SVG per metric into the output directory. The versions are
drawn in the given order. Only the Python standard library is needed.
"""

import math
import os
import sys
from html import escape

TRANSPORTS = ["tcp", "noise", "tls"]
RATHOLE_COLORS = ["#9ecae1", "#4292c6", "#08519c", "#08306b"]
FRP_COLOR = "#e6550d"

WIDTH, HEIGHT = 640, 320
LEFT, RIGHT, TOP, BOTTOM = 64, 16, 64, 40


def number(cell):
    return float(cell.split()[0])


def memory(cell):
    server, client = cell.replace("MiB", "").split("/")
    return float(server) + float(client)


# file name, title, column, parser, unit, format
METRICS = [
    ("bench-throughput.svg", "Throughput (higher is better)", 2, number, "Gbit/s", "{:.1f}"),
    ("bench-latency.svg", "Latency p50 (lower is better)", 3, number, "µs", "{:.0f}"),
    ("bench-connections.svg", "New connections per second (higher is better)", 5, number, "", "{:.0f}"),
    ("bench-memory.svg", "Peak memory, server + client (lower is better)", 6, memory, "MiB", "{:.0f}"),
]


def read_table(path):
    rows, in_table = {}, False
    with open(path, encoding="utf-8") as f:
        for line in f:
            if not line.startswith("|"):
                if in_table:
                    break
                continue
            cells = [c.strip() for c in line.strip().strip("|").split("|")]
            if len(cells) > 1 and cells[1].startswith("Transport"):
                in_table = True
                continue
            if not in_table or cells[0].startswith("---") or cells[2] == "failed":
                continue
            version = cells[0].removesuffix(" *")
            rows[(version, cells[1])] = cells
    return rows


def nice_max(value):
    """Upper end of the axis: 1, 2, 2.5 or 5 times a power of ten"""
    if value <= 0:
        return 1
    exp = 10 ** math.floor(math.log10(value))
    for step in (1, 2, 2.5, 5, 10):
        if value <= step * exp:
            return step * exp
    return 10 * exp


def colors(versions):
    rathole = [v for v in versions if not v.startswith("frp")]
    palette = RATHOLE_COLORS[-len(rathole):] if len(rathole) <= len(RATHOLE_COLORS) else RATHOLE_COLORS
    result, i = {}, 0
    for v in versions:
        if v.startswith("frp"):
            result[v] = FRP_COLOR
        else:
            result[v] = palette[min(i, len(palette) - 1)]
            i += 1
    return result


def chart(rows, versions, title, column, parse, unit, fmt):
    transports = [t for t in TRANSPORTS if any((v, t) in rows for v in versions)]
    values = {k: parse(c[column]) for k, c in rows.items() if k[0] in versions}
    top = nice_max(max(values.values()))
    color = colors(versions)

    plot_w, plot_h = WIDTH - LEFT - RIGHT, HEIGHT - TOP - BOTTOM
    group_w = plot_w / len(transports)
    bar_w = min(36, group_w * 0.8 / len(versions))

    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" '
        f'viewBox="0 0 {WIDTH} {HEIGHT}" font-family="Helvetica, Arial, sans-serif" font-size="12">',
        f'<rect width="{WIDTH}" height="{HEIGHT}" fill="#ffffff"/>',
        f'<text x="{LEFT}" y="22" font-size="15" font-weight="bold" fill="#222">{escape(title)}</text>',
    ]

    # Legend
    x = LEFT
    for v in versions:
        out.append(f'<rect x="{x}" y="34" width="12" height="12" fill="{color[v]}"/>')
        out.append(f'<text x="{x + 16}" y="44" fill="#222">{escape(v)}</text>')
        x += 24 + 7 * len(v)

    # Grid and axis labels
    for i in range(5):
        value = top * i / 4
        y = TOP + plot_h - plot_h * i / 4
        out.append(f'<line x1="{LEFT}" y1="{y:.1f}" x2="{WIDTH - RIGHT}" y2="{y:.1f}" stroke="#ddd"/>')
        label = f"{value:g} {unit}".strip()
        out.append(f'<text x="{LEFT - 6}" y="{y + 4:.1f}" text-anchor="end" fill="#555">{escape(label)}</text>')

    # Bars
    for g, t in enumerate(transports):
        present = [v for v in versions if (v, t) in values]
        start = LEFT + g * group_w + (group_w - bar_w * len(present)) / 2
        for i, v in enumerate(present):
            value = values[(v, t)]
            h = plot_h * value / top
            bx, by = start + i * bar_w, TOP + plot_h - h
            out.append(f'<rect x="{bx + 1:.1f}" y="{by:.1f}" width="{bar_w - 2:.1f}" height="{h:.1f}" fill="{color[v]}"/>')
            out.append(
                f'<text x="{bx + bar_w / 2:.1f}" y="{by - 4:.1f}" text-anchor="middle" font-size="10" fill="#333">'
                f"{escape(fmt.format(value))}</text>"
            )
        cx = LEFT + g * group_w + group_w / 2
        out.append(f'<text x="{cx:.1f}" y="{HEIGHT - BOTTOM + 18}" text-anchor="middle" fill="#222">{t}</text>')

    out.append("</svg>")
    return "\n".join(out) + "\n"


def main():
    if len(sys.argv) < 4:
        sys.exit(__doc__)
    table, out_dir, versions = sys.argv[1], sys.argv[2], sys.argv[3:]
    rows = read_table(table)
    missing = [v for v in versions if not any(k[0] == v for k in rows)]
    if missing:
        sys.exit(f"not in the table: {', '.join(missing)}")
    os.makedirs(out_dir, exist_ok=True)
    for name, title, column, parse, unit, fmt in METRICS:
        path = os.path.join(out_dir, name)
        with open(path, "w", encoding="utf-8") as f:
            f.write(chart(rows, versions, title, column, parse, unit, fmt))
        print(path)


if __name__ == "__main__":
    main()
