# Papier iOS developer utilities.
#
#   make trace                 pull all iOS -> VM -> pi interactions and open HTML
#   make trace DOC=<doc-id>    pull just one document's interaction history
#   make log                   follow the VM's Papier API/service journal
#   make log-pi DOC=<doc-id>   follow pi/transcription telemetry for one document
#
# Override VM when needed, e.g. make trace VM=user@host.

SHELL := /bin/bash

VM ?= exedev@remarkable.exe.xyz
VM_PI_HOME ?= /home/exedev/papier-pi
SSH := ssh -o BatchMode=yes -o ConnectTimeout=8
VM_LOG_LINES ?= 1200

TRACE_DIR := build/vm-trace
TRACE_RAW := $(TRACE_DIR)/raw
TRACE_SESSIONS := $(TRACE_DIR)/sessions
TRACE_METRICS := $(TRACE_DIR)/metrics.jsonl
TRACE_LOG := $(TRACE_DIR)/vm.log
TRACE_HTML := build/trace.html
TRACE_VIEWER := ../papier/tools/trace-viewer.py

.PHONY: trace log log-pi clean-trace

trace:
	@if [[ -n "$(DOC)" && ! "$(DOC)" =~ ^[a-z0-9][a-z0-9_-]{0,100}$$ ]]; then \
		echo 'DOC must be a Papier document id (lowercase letters, digits, _ or -)'; exit 2; \
	fi
	rm -rf $(TRACE_DIR) $(TRACE_HTML)
	mkdir -p $(TRACE_RAW)
	$(SSH) $(VM) 'set -eu; test -d "$(VM_PI_HOME)"; cd "$(VM_PI_HOME)"; scope="$(if $(DOC),./$(DOC),.)"; test -d "$$scope"; find "$$scope" -type f \( -path "*/sessions/*.jsonl" -o -name "metrics.jsonl" -o -name "turn-stderr.log" \) -print0 | tar --null -T - -cf -' \
		| tar -C $(TRACE_RAW) -xf -
	$(SSH) $(VM) 'journalctl -u papier-upload --no-pager -n $(VM_LOG_LINES) -o short-iso 2>/dev/null || true' \
		> $(TRACE_RAW)/papier-upload.journal.log
	python3 tools/prepare-trace.py $(TRACE_RAW) $(TRACE_DIR)
	python3 $(TRACE_VIEWER) $(TRACE_SESSIONS) --log $(TRACE_LOG) --metrics $(TRACE_METRICS) \
		--title "Papier iOS · Pi interaction traces" -o $(TRACE_HTML)
	@open $(TRACE_HTML) 2>/dev/null || true
	@echo "Papier iOS trace: $(abspath $(TRACE_HTML))"

log:
	$(SSH) $(VM) 'journalctl -u papier-upload --no-pager -n 100 -f -o short-iso'

log-pi:
	@if [[ ! "$(DOC)" =~ ^[a-z0-9][a-z0-9_-]{0,100}$$ ]]; then \
		echo 'usage: make log-pi DOC=<document-id> [VM=user@host]'; exit 2; \
	fi
	$(SSH) $(VM) 'test -f "$(VM_PI_HOME)/$(DOC)/turn-stderr.log" || { echo "no Pi log for $(DOC)" >&2; exit 1; }; tail -n 100 -F "$(VM_PI_HOME)/$(DOC)/turn-stderr.log"'

clean-trace:
	rm -rf $(TRACE_DIR) $(TRACE_HTML)
