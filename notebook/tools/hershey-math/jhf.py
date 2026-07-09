"""Parse Hershey .jhf files (with continuation lines) into glyphs."""


def parse_jhf(path):
    glyphs = []  # list of (glyphnum, left, right, [ (x,y) | None ])
    with open(path) as f:
        raw = [ln.rstrip("\n") for ln in f if ln.strip()]
    i = 0
    while i < len(raw):
        line = raw[i]
        i += 1
        num = int(line[0:5])
        nverts = int(line[5:8])  # pairs incl. the margin pair
        data = line[8:]
        # continuation lines until we have nverts pairs (2 chars each)
        while len(data) < (nverts) * 2 and i < len(raw):
            data += raw[i]
            i += 1
        left = ord(data[0]) - ord("R")
        right = ord(data[1]) - ord("R")
        pts = []
        for j in range(1, nverts):
            cx, cy = data[2 * j], data[2 * j + 1]
            if cx == " " and cy == "R":
                pts.append(None)  # pen up
            else:
                pts.append((ord(cx) - ord("R"), ord(cy) - ord("R")))
        glyphs.append((num, left, right, pts))
    return glyphs


def polylines(pts):
    out, cur = [], []
    for p in pts:
        if p is None:
            if len(cur) >= 2:
                out.append(cur)
            cur = []
        else:
            cur.append(p)
    if len(cur) >= 2:
        out.append(cur)
    return out
