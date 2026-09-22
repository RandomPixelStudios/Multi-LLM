#!/bin/sh
# Lokalen API-Key eines Benutzers ausgeben (für /v1-API-Clients).
# Aufruf: ./get-key.sh anna
set -eu
USER="${1:?Benutzername fehlt, z. B. ./get-key.sh anna}"
docker exec multillm cat "/data/users/$USER/secrets.json" \
  | python3 -c "import json,sys; print(json.load(sys.stdin)['local-api-key'])"
