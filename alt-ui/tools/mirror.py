#!/usr/bin/env python3
"""Mirror PDFs from the tablet's own library (xochitl) into reader books.

The reMarkable stores documents as <uuid>.pdf + <uuid>.metadata under
~/.local/share/remarkable/xochitl. This tool lists them over ssh, pulls the
ones you pick to the desk side, renders them with mkbook, and pushes the
bundles back — so anything you already sent to the tablet the normal way
(app / web / drag-drop) can appear on the reader shelf too.

    uv run --with pymupdf --with numpy python3 tools/mirror.py root@<ip> --list
    ... --list --only atten          # filter the listing
    ... --only "attention is all"    # import everything matching (<= 10)
    ... --only muon --force          # re-render even if already imported
    ... --all                        # no 10-doc safety cap

Selective by design: a tablet easily holds hundreds of PDFs; mirror what
you are actually reading. Already-imported slugs are skipped (their ink,
notes and position stay untouched) unless --force. xochitl's own pen
annotations are NOT converted — books arrive with fresh margins.
"""
import argparse
import json
import os
import re
import subprocess
import sys

import mkbook

XOCHITL = "/home/root/.local/share/remarkable/xochitl"
BOOKS = "/home/root/.local/share/alt-ui/docs"

LIST_SCRIPT = f"""
cd {XOCHITL} 2>/dev/null || exit 0
for m in *.metadata; do
  u="${{m%.metadata}}"
  [ -f "$u.pdf" ] || continue
  sz=$(du -k "$u.pdf" | cut -f1)
  echo "@@@ $u $sz"
  cat "$m"
done
"""


def ssh(host, script, binary=False):
    r = subprocess.run(["ssh", "-o", "ConnectTimeout=8", host, script],
                       capture_output=True, check=True)
    return r.stdout if binary else r.stdout.decode("utf-8", "replace")


def slugify(name):
    s = re.sub(r"\.pdf$", "", name, flags=re.I).lower()
    s = re.sub(r"[^a-z0-9]+", "-", s).strip("-")
    return s[:60].strip("-") or "book"


def fetch_docs(host):
    docs = []
    cur = None
    for line in ssh(host, LIST_SCRIPT).splitlines():
        if line.startswith("@@@ "):
            _, uuid, kb = line.split()
            cur = {"uuid": uuid, "kb": int(kb), "raw": []}
            docs.append(cur)
        elif cur is not None:
            cur["raw"].append(line)
    out = []
    for d in docs:
        try:
            meta = json.loads("\n".join(d["raw"]))
        except ValueError:
            continue
        if meta.get("deleted") or meta.get("parent") == "trash":
            continue
        if meta.get("type") not in (None, "DocumentType"):
            continue
        name = (meta.get("visibleName") or d["uuid"]).strip()
        out.append({
            "uuid": d["uuid"],
            "kb": d["kb"],
            "name": name,
            "slug": slugify(name),
            "mtime": int(meta.get("lastModified") or 0),
        })
    out.sort(key=lambda d: -d["mtime"])
    # de-collide slugs (two docs named the same)
    seen = {}
    for d in out:
        if d["slug"] in seen:
            d["slug"] = f"{d['slug']}-{d['uuid'][:8]}"
        seen[d["slug"]] = True
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("host")
    ap.add_argument("--only", help="case-insensitive substring of the document name")
    ap.add_argument("--list", action="store_true", help="just show what is on the tablet")
    ap.add_argument("--force", action="store_true", help="re-import even if the slug exists")
    ap.add_argument("--all", action="store_true", help="lift the 10-document safety cap")
    ap.add_argument("--margin", type=int, default=40,
                    help="white border in device px (see mkbook; default 40)")
    ap.add_argument("--margin-left", type=int)
    ap.add_argument("--margin-top", type=int)
    ap.add_argument("--margin-right", type=int)
    ap.add_argument("--margin-bottom", type=int)
    args = ap.parse_args()

    docs = fetch_docs(args.host)
    existing = set(ssh(args.host, f"ls {BOOKS} 2>/dev/null").split())
    if args.only:
        needle = args.only.lower()
        docs = [d for d in docs if needle in d["name"].lower()]

    if args.list or not args.only and not args.all:
        shown = docs if (args.only or args.all) else docs[:40]
        for d in shown:
            mark = " [imported]" if d["slug"] in existing else ""
            print(f"{d['kb']:>8} KB  {d['name']}{mark}")
        if len(shown) < len(docs):
            print(f"... and {len(docs) - len(shown)} more (use --only to filter)")
        if not args.list and not args.only:
            print("\nmirror: pass ONLY=\"name fragment\" to import "
                  "(this tablet has too many PDFs to mirror blindly)", file=sys.stderr)
            return 1
        return 0

    todo = [d for d in docs if args.force or d["slug"] not in existing]
    skipped = len(docs) - len(todo)
    if not todo:
        print(f"mirror: nothing to do ({skipped} already imported)")
        return 0
    if len(todo) > 10 and not args.all:
        print(f"mirror: {len(todo)} documents match — narrow --only or pass --all:", file=sys.stderr)
        for d in todo:
            print(f"  {d['kb']:>8} KB  {d['name']}", file=sys.stderr)
        return 1

    os.makedirs("build/mirror/books", exist_ok=True)
    for d in todo:
        print(f"mirror: pulling '{d['name']}' ({d['kb']} KB)...")
        pdf = ssh(args.host, f"cat {XOCHITL}/{d['uuid']}.pdf", binary=True)
        local = f"build/mirror/{d['slug']}.pdf"
        with open(local, "wb") as f:
            f.write(pdf)
        out = f"build/mirror/books/{d['slug']}"
        try:
            mkbook.build_book(local, out, d["name"].removesuffix(".pdf"), margin=args.margin,
                              margins=(args.margin_left, args.margin_top,
                                       args.margin_right, args.margin_bottom))
        except Exception as e:
            print(f"mirror: SKIPPING '{d['name']}': {e}", file=sys.stderr)
            continue
        subprocess.run(
            ["bash", "-c",
             f"tar -C build/mirror/books -cf - {d['slug']!r} | "
             f"ssh {args.host} 'mkdir -p {BOOKS} && tar -C {BOOKS} -xf -'"],
            check=True)
        print(f"mirror: '{d['name']}' -> {d['slug']}")
    if skipped:
        print(f"mirror: skipped {skipped} already imported (use --force to redo)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
