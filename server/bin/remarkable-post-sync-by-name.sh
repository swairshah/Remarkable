#!/usr/bin/env bash
set -euo pipefail

DOC_NAME="${1:-Notebook}"
BASE="/home/swair/remarkable-backup/xochitl"
OUT_BASE="/home/swair/remarkable-exports"

UUID="$(python3 - "$BASE" "$DOC_NAME" <<'PY'
import json,glob,os,sys
base,name = sys.argv[1],sys.argv[2].lower()
best=None
for p in glob.glob(os.path.join(base, '*.metadata')):
    try:
        j=json.load(open(p,'r',encoding='utf-8'))
    except Exception:
        continue
    if (j.get('visibleName','').lower() != name):
        continue
    lm=int(j.get('lastModified','0') or 0)
    u=os.path.basename(p).replace('.metadata','')
    if (best is None) or (lm > best[0]):
        best=(lm,u,j.get('visibleName','Notebook'))
if not best:
    print('')
else:
    print(best[1])
PY
)"

if [[ -z "$UUID" ]]; then
  echo "No document found with visibleName='$DOC_NAME'" >&2
  exit 0
fi

SRC_THUMBS="$BASE/$UUID.thumbnails"
SRC_CONTENT="$BASE/$UUID.content"
SRC_META="$BASE/$UUID.metadata"
SAFE_NAME="$(echo "$DOC_NAME" | tr ' /' '__')"
OUT_DIR="$OUT_BASE/$SAFE_NAME"
PAGES_DIR="$OUT_DIR/pages_png"
JPG_DIR="$OUT_DIR/pages_jpg"
PDF_OUT="$OUT_DIR/${SAFE_NAME}.pdf"
LOG="$OUT_DIR/export.log"

mkdir -p "$PAGES_DIR" "$JPG_DIR"
echo "[$(date -Is)] export start doc='$DOC_NAME' uuid=$UUID" >> "$LOG"

if [[ ! -f "$SRC_CONTENT" || ! -d "$SRC_THUMBS" ]]; then
  echo "[$(date -Is)] missing content/thumbnails for uuid=$UUID" >> "$LOG"
  exit 0
fi

cp -f "$SRC_CONTENT" "$OUT_DIR/document.content"
cp -f "$SRC_META" "$OUT_DIR/document.metadata" 2>/dev/null || true

python3 - "$SRC_CONTENT" "$SRC_THUMBS" "$PAGES_DIR" <<'PY'
import json,sys,os,shutil,glob
content,thumbs,out = sys.argv[1:4]
for f in glob.glob(os.path.join(out,'*.png')):
    os.remove(f)
with open(content,'r',encoding='utf-8') as f:
    j=json.load(f)
pages=j.get('cPages',{}).get('pages',[])
count=0
for i,p in enumerate(pages,1):
    pid=p.get('id')
    if not pid:
        continue
    src=os.path.join(thumbs,f'{pid}.png')
    if not os.path.exists(src):
        continue
    dst=os.path.join(out,f'{i:03d}-{pid}.png')
    shutil.copy2(src,dst)
    count+=1
print(f'exported_png={count}')
PY

if command -v magick >/dev/null 2>&1; then
  find "$JPG_DIR" -type f -name '*.jpg' -delete
  find "$PAGES_DIR" -type f -name '*.png' | sort | while read -r p; do
    b="$(basename "$p" .png)"
    magick "$p" -quality 92 "$JPG_DIR/$b.jpg"
  done
elif command -v convert >/dev/null 2>&1; then
  find "$JPG_DIR" -type f -name '*.jpg' -delete
  find "$PAGES_DIR" -type f -name '*.png' | sort | while read -r p; do
    b="$(basename "$p" .png)"
    convert "$p" -quality 92 "$JPG_DIR/$b.jpg"
  done
fi

if command -v img2pdf >/dev/null 2>&1; then
  if ls "$PAGES_DIR"/*.png >/dev/null 2>&1; then
    img2pdf $(ls "$PAGES_DIR"/*.png | sort) -o "$PDF_OUT"
  fi
fi

echo "[$(date -Is)] export done doc='$DOC_NAME' uuid=$UUID" >> "$LOG"
