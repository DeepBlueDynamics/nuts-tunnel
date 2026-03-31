#!/bin/bash
set -e

# Start auth service in background
cd /auth && uvicorn main:app --host 127.0.0.1 --port 9000 --log-level info &

# Small delay for auth to bind
sleep 1

# Start proxy in foreground
exec nuts-proxy
