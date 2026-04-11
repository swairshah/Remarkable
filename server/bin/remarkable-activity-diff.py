#!/usr/bin/env python3
import json
import hashlib
from pathlib import Path
from datetime import datetime, timezone

BASE = Path('/home/swair/remarkable-backup/xochitl')
OUT = Path('/home/swair/remarkable-exports/activity')
STATE = OUT / 'last_state.json'
LATEST = OUT / 'latest.txt'


def read_json(path: Path):
    try:
        return json.loads(path.read_text(encoding='utf-8'))
    except Exception:
        return None


def sha256_file(path: Path):
    h = hashlib.sha256()
    try:
        with path.open('rb') as f:
            for chunk in iter(lambda: f.read(1024 * 1024), b''):
                h.update(chunk)
        return h.hexdigest()
    except Exception:
        return ''


def ms_to_iso(ms):
    try:
        ms_i = int(str(ms))
        return datetime.fromtimestamp(ms_i / 1000, tz=timezone.utc).isoformat()
    except Exception:
        return ''


def build_state():
    docs = {}
    for m in BASE.glob('*.metadata'):
        uuid = m.name[:-9]
        md = read_json(m)
        if not isinstance(md, dict):
            continue

        c = BASE / f'{uuid}.content'
        bookm = BASE / f'{uuid}.bookm'
        hdir = BASE / f'{uuid}.highlights'
        rm_dir = BASE / uuid

        # bookmark info
        bookm_count = 0
        if bookm.exists():
            bj = read_json(bookm)
            if isinstance(bj, dict):
                bookm_count = len(bj.keys())

        # highlights info
        highlight_files = []
        if hdir.exists() and hdir.is_dir():
            highlight_files = sorted([p for p in hdir.glob('*.json') if p.is_file()])

        hl_hash_input = '|'.join(f'{p.name}:{int(p.stat().st_mtime)}:{p.stat().st_size}' for p in highlight_files)
        hl_hash = hashlib.sha256(hl_hash_input.encode('utf-8')).hexdigest() if highlight_files else ''

        # notebook stroke activity
        rm_count = 0
        rm_latest_mtime = 0
        if rm_dir.exists() and rm_dir.is_dir():
            for p in rm_dir.glob('*.rm'):
                rm_count += 1
                mt = int(p.stat().st_mtime)
                if mt > rm_latest_mtime:
                    rm_latest_mtime = mt

        docs[uuid] = {
            'uuid': uuid,
            'name': md.get('visibleName', ''),
            'type': md.get('type', ''),
            'parent': md.get('parent', ''),
            'deleted': bool(md.get('deleted', False)),
            'lastModified': str(md.get('lastModified', '0')),
            'lastOpened': str(md.get('lastOpened', '0')),
            'lastOpenedPage': md.get('lastOpenedPage', None),
            'metadata_hash': sha256_file(m),
            'content_hash': sha256_file(c) if c.exists() else '',
            'bookm_exists': bookm.exists(),
            'bookm_count': bookm_count,
            'bookm_hash': sha256_file(bookm) if bookm.exists() else '',
            'highlights_count': len(highlight_files),
            'highlights_hash': hl_hash,
            'rm_page_count': rm_count,
            'rm_latest_mtime': rm_latest_mtime,
        }

    return {
        'generated_at': datetime.now(tz=timezone.utc).isoformat(),
        'doc_count': len(docs),
        'docs': docs,
    }


def load_prev():
    if not STATE.exists():
        return None
    try:
        return json.loads(STATE.read_text(encoding='utf-8'))
    except Exception:
        return None


def compare(prev, cur):
    changes = []
    prev_docs = prev.get('docs', {}) if prev else {}
    cur_docs = cur.get('docs', {})

    for uuid, d in cur_docs.items():
        p = prev_docs.get(uuid)
        if p is None:
            continue

        name = d.get('name') or uuid
        kind = d.get('type', '')

        line_bits = []

        # Reading progress
        op_old = p.get('lastOpenedPage')
        op_new = d.get('lastOpenedPage')
        if op_old != op_new and (op_old is not None or op_new is not None):
            line_bits.append(f'page {op_old} -> {op_new}')

        # Opened/modified times
        if d.get('lastOpened') != p.get('lastOpened'):
            line_bits.append('opened')
        if d.get('lastModified') != p.get('lastModified'):
            line_bits.append('modified')

        # Bookmarks
        if d.get('bookm_hash') != p.get('bookm_hash'):
            line_bits.append(f'bookmarks {p.get("bookm_count",0)} -> {d.get("bookm_count",0)}')

        # Highlights
        if d.get('highlights_hash') != p.get('highlights_hash'):
            line_bits.append(f'highlights {p.get("highlights_count",0)} -> {d.get("highlights_count",0)}')

        # Notebook stroke changes
        if d.get('rm_latest_mtime') != p.get('rm_latest_mtime'):
            line_bits.append('handwriting changed')

        if line_bits:
            changes.append({
                'uuid': uuid,
                'name': name,
                'type': kind,
                'bits': line_bits,
                'lastModified': d.get('lastModified', '0'),
            })

    # sort by latest modification desc
    def lm(x):
        try:
            return int(str(x.get('lastModified', '0')))
        except Exception:
            return 0

    changes.sort(key=lm, reverse=True)
    return changes


def write_report(changes, cur):
    OUT.mkdir(parents=True, exist_ok=True)
    now = datetime.now(tz=timezone.utc)
    stamp = now.strftime('%Y%m%d-%H%M%S')
    report_path = OUT / f'activity-{stamp}.txt'

    lines = []
    lines.append(f'reMarkable Activity Diff')
    lines.append(f'Generated: {now.isoformat()}')
    lines.append(f'Documents scanned: {cur.get("doc_count",0)}')
    lines.append('')

    if not changes:
        lines.append('No activity changes since last sync.')
    else:
        lines.append(f'Changes since last sync: {len(changes)} documents')
        lines.append('')
        for c in changes:
            lines.append(f'- {c["name"]} [{c["type"]}] ({c["uuid"]})')
            for b in c['bits']:
                lines.append(f'  - {b}')

    txt = '\n'.join(lines) + '\n'
    report_path.write_text(txt, encoding='utf-8')
    LATEST.write_text(txt, encoding='utf-8')
    return report_path


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    prev = load_prev()
    cur = build_state()

    # Always persist current state for next run
    STATE.write_text(json.dumps(cur, indent=2), encoding='utf-8')

    if prev is None:
        msg = 'Baseline created (no previous state). Diffing starts next sync.\n'
        LATEST.write_text(msg, encoding='utf-8')
        print(msg.strip())
        return 0

    changes = compare(prev, cur)
    report = write_report(changes, cur)
    if changes:
        print(f'Activity changes found: {len(changes)} (report: {report})')
    else:
        print('No activity changes since last sync.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
