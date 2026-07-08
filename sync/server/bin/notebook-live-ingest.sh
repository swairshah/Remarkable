#!/usr/bin/env bash
# Run over SSH by the tablet's notebook app when LIVE is toggled on:
# forwards the JSONL stroke stream from stdin into the relay's ingest port.
# Fails fast if the relay is down so the tablet's reconnect loop kicks in.
exec 3>/dev/tcp/127.0.0.1/8092 || exit 1
exec cat >&3
