#!/bin/bash

# Serve the web folder using Python's built-in HTTP server
# Default port is 8000, or pass a custom port as first argument

PORT=${1:-8000}
DIR="web"

if [ ! -d "$DIR" ]; then
    echo "Error: '$DIR' directory not found"
    exit 1
fi

echo "Serving '$DIR' at http://localhost:$PORT"
python3 -m http.server "$PORT" --directory "$DIR"


