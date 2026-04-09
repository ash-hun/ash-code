#!/usr/bin/env bash
set -euo pipefail

# ash-code container entrypoint.
# If the caller invoked the image with a custom command, honor it.
# Otherwise start supervisord which runs ashpy + ash serve together.

if [[ "${1:-}" == "supervisord" ]]; then
    exec /usr/bin/supervisord -c /etc/supervisor/conf.d/ash.conf
fi
exec "$@"
