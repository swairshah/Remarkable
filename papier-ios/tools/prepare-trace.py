#!/usr/bin/env python3
"""Prepare VM-side Papier Pi logs for the shared HTML trace viewer.

The VM keeps one directory per document:

    <doc>/sessions/*.jsonl
    <doc>/metrics.jsonl
    <doc>/turn-stderr.log

This script flattens active sessions with a document-id prefix, merges and
chronologically sorts metrics, and combines the service journal with each
Pi stderr stream. A partially-written final JSONL line is ignored so pulling
while Pi is answering remains safe.
"""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path


def document_id(path: Path, raw: Path) -> str:
    rel = path.relative_to(raw)
    return rel.parts[0] if len(rel.parts) > 1 else "unknown"


def read_json_lines(path: Path) -> tuple[list[dict], int]:
    records: list[dict] = []
    skipped = 0
    try:
        lines = path.read_text(errors="replace").splitlines()
    except OSError:
        return records, skipped
    for line in lines:
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except (TypeError, ValueError):
            skipped += 1
            continue
        if isinstance(value, dict):
            records.append(value)
    return records, skipped


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("raw", type=Path, help="raw tree copied from VM papier-pi")
    parser.add_argument("out", type=Path, help="prepared trace output directory")
    args = parser.parse_args()

    raw = args.raw.resolve()
    out = args.out.resolve()
    sessions_out = out / "sessions"
    sessions_out.mkdir(parents=True, exist_ok=True)

    session_files = sorted(raw.glob("*/sessions/*.jsonl"))
    for src in session_files:
        doc = document_id(src, raw)
        shutil.copy2(src, sessions_out / f"{doc}--{src.name}")

    metrics: list[dict] = []
    skipped_metrics = 0
    metric_files = sorted(raw.glob("*/metrics.jsonl"))
    for src in metric_files:
        doc = document_id(src, raw)
        records, skipped = read_json_lines(src)
        skipped_metrics += skipped
        for record in records:
            record = {"doc": doc, **record}
            metrics.append(record)
    metrics.sort(key=lambda record: str(record.get("ts", "")))
    metrics_path = out / "metrics.jsonl"
    with metrics_path.open("w") as stream:
        for record in metrics:
            stream.write(json.dumps(record, separators=(",", ":")) + "\n")

    log_chunks: list[str] = []
    journal = raw / "papier-upload.journal.log"
    if journal.exists():
        log_chunks.append("=== papier-upload service journal ===\n" + journal.read_text(errors="replace").rstrip())

    stderr_files = sorted(raw.glob("*/turn-stderr.log"))
    for src in stderr_files:
        doc = document_id(src, raw)
        log_chunks.append(f"=== Pi stderr · {doc} ===\n" + src.read_text(errors="replace").rstrip())
    log_path = out / "vm.log"
    log_path.write_text("\n\n".join(chunk for chunk in log_chunks if chunk.strip()) + "\n")

    docs = sorted({document_id(path, raw) for path in session_files + metric_files + stderr_files})
    print(
        "prepare-trace: "
        f"{len(docs)} document(s), {len(session_files)} session(s), "
        f"{len(metrics)} metric event(s), {len(stderr_files)} stderr log(s)"
        + (f", skipped {skipped_metrics} partial metric line(s)" if skipped_metrics else "")
    )
    if not session_files:
        raise SystemExit("prepare-trace: no active Pi session JSONL files found on the VM")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
