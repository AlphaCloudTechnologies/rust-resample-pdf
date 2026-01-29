#!/bin/bash
set -e

echo "🔧 Building PDF Resampler for WebAssembly..."
echo ""

# Check if wasm-pack is installed
if ! command -v wasm-pack &> /dev/null; then
    echo "❌ wasm-pack is not installed."
    echo ""
    echo "Install it with:"
    echo "  cargo install wasm-pack"
    echo ""
    echo "Or visit: https://rustwasm.github.io/wasm-pack/installer/"
    exit 1
fi

# Build WASM module
echo "📦 Building WASM module..."
RUSTFLAGS='--cfg getrandom_backend="wasm_js"' wasm-pack build --target web --out-dir web/pkg --release

# Clean up unnecessary files
echo "🧹 Cleaning up..."
rm -f web/pkg/.gitignore
rm -f web/pkg/package.json
rm -f web/pkg/README.md

echo ""
echo "✅ Build complete!"
echo ""
echo "To serve the web app locally, you can use:"
echo "  cd web && python3 -m http.server 8080"
echo ""
echo "Then open http://localhost:8080 in your browser."

