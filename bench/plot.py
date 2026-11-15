"""Draw docs/img/includes.svg from bench/includes.json (no plotting library needed).

    python3 bench/plot.py
"""
import json
import pathlib

root = pathlib.Path(__file__).resolve().parent.parent
rows = json.loads((root / "bench" / "includes.json").read_text())

bar_h, gap, left, width = 22, 34, 150, 430
top = 46
height = top + len(rows) * (2 * bar_h + gap) + 30
longest = max(r["loop"]["ms"] for r in rows)
scale = width / longest

parts = [
    f'<svg xmlns="http://www.w3.org/2000/svg" width="{left + width + 220}" height="{height}" '
    'font-family="system-ui, sans-serif" font-size="13">',
    '<style>text{fill:#57606a}.v{fill:#24292f}@media (prefers-color-scheme: dark)'
    '{text{fill:#8b949e}.v{fill:#e6edf3}}</style>',
    '<text x="0" y="18" class="v" font-size="15" font-weight="600">'
    "Users with their posts: median time, and statements sent</text>",
    f'<rect x="{left}" y="26" width="12" height="12" fill="#0969da"/>'
    f'<text x="{left + 18}" y="37">include_posts()</text>'
    f'<rect x="{left + 140}" y="26" width="12" height="12" fill="#bf8700"/>'
    f'<text x="{left + 158}" y="37">a query per user</text>',
]
y = top
for r in rows:
    parts.append(
        f'<text x="0" y="{y + bar_h + 6}" class="v">{r["users"]:,} users</text>'
        f'<text x="0" y="{y + bar_h + 22}">{r["posts"]:,} posts</text>'
    )
    for key, colour in (("include", "#0969da"), ("loop", "#bf8700")):
        ms, statements = r[key]["ms"], r[key]["statements"]
        w = max(2.0, ms * scale)
        parts.append(
            f'<rect x="{left}" y="{y}" width="{w:.1f}" height="{bar_h - 4}" rx="2" fill="{colour}"/>'
            f'<text x="{left + w + 6:.1f}" y="{y + bar_h - 9}" class="v">{ms:.2f} ms · '
            f'{statements:,} statements</text>'
        )
        y += bar_h
    y += gap
parts.append("</svg>")
out = root / "docs" / "img" / "includes.svg"
out.write_text("\n".join(parts) + "\n")
print(f"wrote {out.relative_to(root)}")
