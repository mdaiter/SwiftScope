# Default knobs for the autonomous device loop.
DEVICE              ?=
BUNDLE_ID           ?=
APP_BUNDLE          ?=
APP_PROGRAM         ?=
BRIDGE_PORT         ?= 50001
API_PORT            ?= 4000
DEBUGSERVER_PATH    ?= /Developer/usr/libexec/debugserver
DEVICETCL           ?= xcrun
DEVICETCTL_SUBCMD   ?= devicectl
LAUNCH_ARGS         ?=
STATE_FILE          ?= .zed/ios-llm-state.json

.PHONY: autonomy
autonomy:
	@if [ -z "$(DEVICE)" ]; then echo "Set DEVICE=<udid|name> before running make autonomy"; exit 1; fi
	@if [ -z "$(BUNDLE_ID)" ]; then echo "Set BUNDLE_ID=<com.example.App>"; exit 1; fi
	@bash -lc 'set -euo pipefail; \
	app_program="$(APP_PROGRAM)"; \
	if [ -z "$$app_program" ] && [ -f "$(STATE_FILE)" ]; then \
		app_program=$$(python - <<'"'"'PY'"'"' || true
import json, sys
from pathlib import Path
path = Path("$(STATE_FILE)")
try:
    data = json.loads(path.read_text())
    print(data.get("app_binary", ""))
except FileNotFoundError:
    print("")
PY
); \
	fi; \
	if [ -z "$$app_program" ]; then \
		echo "Set APP_PROGRAM or ensure $(STATE_FILE) exists with app_binary"; \
		exit 1; \
	fi; \
	echo "Using program $$app_program"; \
	bridge_cmd=(cargo run --features cli --bin ios-llm-devicectl -- \
		--device "$(DEVICE)" \
		--bundle-id "$(BUNDLE_ID)" \
		--listen-port $(BRIDGE_PORT) \
		--debugserver-path "$(DEBUGSERVER_PATH)" \
		--devicectl "$(DEVICETCL)" \
		--devicectl-subcommand "$(DEVICETCTL_SUBCMD)"); \
	if [ -n "$(APP_BUNDLE)" ]; then \
		bridge_cmd+=(--install-app "$(APP_BUNDLE)"); \
	fi; \
	if [ -n "$(LAUNCH_ARGS)" ]; then \
		for arg in $(LAUNCH_ARGS); do \
			bridge_cmd+=(--launch-arg "$$arg"); \
		done; \
		fi; \
		"${bridge_cmd[@]}" & \
		bridge_pid=$$!; \
		trap "kill $$bridge_pid >/dev/null 2>&1 || true" EXIT INT TERM; \
		cargo run --features cli --bin ios_llm_api -- \
			--manage-bridge \
			--device "$(DEVICE)" \
			--bundle-id "$(BUNDLE_ID)" \
			--state-file "$(STATE_FILE)" \
			--debugserver-port $(BRIDGE_PORT) \
			--program "$$app_program" \
			--port $(API_PORT) & \
		api_pid=$$!; \
		until curl -sf "http://127.0.0.1:$(API_PORT)/health" >/dev/null 2>&1; do \
			sleep 1; \
		done; \
		echo "ios_llm_api listening on port $(API_PORT)"; \
		wait $$api_pid;'
