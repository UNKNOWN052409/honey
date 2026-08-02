import json, re, sys

with open(sys.argv[1]) as f:
    data = json.load(f)
d = data["data"][0]

TEST_MOD_LINE = int(sys.argv[2]) if len(sys.argv) > 2 else 1590

def demangle(n):
    n = re.sub(r"^_[A-Z0-9]+", "", n)
    n = re.sub(r"^\d+", "", n)
    n = re.sub(r"[0-9A-F_]{2,}$", "", n)
    return n

rows = []
for fn in d["functions"]:
    raw = fn.get("name", "")
    regs = fn.get("regions", [])
    if not regs:
        continue
    line = min(r[0] for r in regs)
    if line >= TEST_MOD_LINE:
        continue
    total = len(regs)
    hit = sum(1 for r in regs if r[4] > 0)
    rows.append((hit, total, demangle(raw), line))

rows.sort(key=lambda r: r[1] - r[0])
out = []
out.append("%5s %4s/%4s  %5s  %s" % ("miss", "hit", "tot", "line", "func"))
for hit, tot, name, line in rows:
    if tot >= 6:
        out.append("%4d %4d/%4d  %5d  %s" % (tot - hit, hit, tot, line, name))
print("\n".join(out))
