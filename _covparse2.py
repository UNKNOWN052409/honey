import json, sys, re

def demangle(name):
    # keep the last readable part: hg_supervisor::fnname
    m = re.search(r'13hg_supervisor(\d+[a-zA-Z0-9_]+)', name)
    if m:
        s = m.group(1)
        # leading digits = string length
        s = re.sub(r'^\d+', '', s)
        return s
    return name

with open(sys.argv[1]) as f:
    data = json.load(f)

test_mod_line = int(sys.argv[2]) if len(sys.argv) > 2 else 999999

for fobj in data.get("data", []):
    funcs = []
    for fn in fobj.get("functions", []):
        fname = fn.get("name", "")
        regs = fn.get("regions", [])
        start = regs[0][0] if regs and len(regs[0]) >= 5 else fn.get("start_line", 0)
        if start >= test_mod_line:
            continue
        t = 0
        h = 0
        for r in regs:
            if len(r) < 5:
                continue
            lines = r[2] - r[0] + 1
            if lines < 1:
                continue
            t += lines
            if r[4] > 0:
                h += lines
        if t == 0:
            continue
        funcs.append((start, demangle(fname), h, t - h, t))
    funcs.sort()
    for start, fname, hit, miss, total in funcs:
        pct = 100.0 * hit / total
        print(f"  :{start:5d} {pct:6.1f}% hit={hit:4d} miss={miss:4d} tot={total:4d}  {fname}")
