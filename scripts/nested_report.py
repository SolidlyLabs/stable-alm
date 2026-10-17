"""Score the nested-range grid (alm/grid.py: nested_grid) against the live ladder family, with the same
walk-forward and bootstrap as scripts/ideas_report.py. Writes reports/nested_report.md."""
import os
import re
import sys

sys.path.insert(0, os.path.dirname(__file__))
import ideas_report  # noqa: E402


def family(c: str) -> str:
    if c.startswith("ladder("):
        return ideas_report.BASE
    if c.startswith("nested("):
        return "ladder as nested ranges"
    if c.startswith("pyramid("):
        ks = re.search(r"ks=\(([^)]*)\)", c).group(1)
        widest = max(int(x) for x in ks.split(",") if x.strip())
        a = re.search(r"a=([0-9.]+)", c).group(1)
        return f"pyramid {'narrow (<=±2)' if widest <= 2 else 'wide (±3-6)'} a={a}"
    return "other"


def to_markdown(df, index=True, **_):
    """pandas' to_markdown without the tabulate dependency."""
    d = df.reset_index() if index else df
    rows = [[str(c) for c in d.columns]] + [[str(v) for v in r] for r in d.itertuples(index=False)]
    lines = ["| " + " | ".join(rows[0]) + " |", "|" + "---|" * len(rows[0])]
    return "\n".join(lines + ["| " + " | ".join(r) + " |" for r in rows[1:]])


ideas_report.pd.DataFrame.to_markdown = to_markdown
ideas_report.family = family
sys.argv = [sys.argv[0], "nested"]
ideas_report.main()
