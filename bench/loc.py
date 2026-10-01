"""Lines of code without blank lines, comments and docstrings.

    python bench/loc.py FILE...
"""
import io
import sys
import tokenize


def py_lines(path: str) -> int:
    src = open(path).read()
    lines: set[int] = set()
    prev = tokenize.INDENT
    for tok in tokenize.generate_tokens(io.StringIO(src).readline):
        kind = tok.type
        is_doc = kind == tokenize.STRING and prev in (tokenize.INDENT, tokenize.NEWLINE, tokenize.DEDENT)
        if kind not in (tokenize.COMMENT, tokenize.NL, tokenize.NEWLINE, tokenize.INDENT,
                        tokenize.DEDENT, tokenize.ENDMARKER) and not is_doc:
            lines.update(range(tok.start[0], tok.end[0] + 1))
        if kind not in (tokenize.COMMENT, tokenize.NL):
            prev = kind
    return len(lines)


def clyx_lines(path: str) -> int:
    return sum(1 for line in open(path) if line.strip() and not line.strip().startswith("#"))


for p in sys.argv[1:]:
    n = clyx_lines(p) if p.endswith(".clyx") else py_lines(p)
    print(f"{n:5}  {p}")
