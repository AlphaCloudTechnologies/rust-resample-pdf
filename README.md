# resample-pdf

A fast Rust tool for downsampling images in PDF files. Available as a CLI and browser-based web app. Shrinks bloated PDFs by resampling high-DPI images to a target resolution while preserving visual quality.

**🌐 [Live Demo](https://apitemplate.io/pdf-tools/compress-pdf-online-for-free/)** — Try it in your browser, no installation required.

## Features

- **DPI-aware resampling** — calculates effective DPI from display dimensions, not just pixel count
- **Selective processing** — only touches images above the target threshold
- **Alpha preservation** — handles transparency correctly via SMask
- **Deep scanning** — finds images in pages, Form XObjects, annotations, tiling patterns, and soft masks
- **JPEG output** — 4:2:0 chroma subsampling for optimal compression
- **Cross-platform** — runs as CLI or in browser via WebAssembly
- **Privacy-first** — web version processes files entirely in your browser

## Installation

### CLI

```bash
cargo build --release
```

Binary: `target/release/resample-pdf`

### WebAssembly (Browser)

Build the WASM module:

```bash
# Install wasm-pack if you don't have it
cargo install wasm-pack

# Build WASM module
./build-wasm.sh
```

Serve the web app:

```bash
cd web && python3 -m http.server 8080
```

Open http://localhost:8080 in your browser.

## Usage

### CLI

```bash
resample-pdf -i input.pdf -o output.pdf [OPTIONS]
```

### Options

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--input` | `-i` | — | Input PDF file (required) |
| `--output` | `-o` | — | Output PDF file (required) |
| `--dpi` | `-d` | 150 | Target DPI |
| `--quality` | `-q` | 75 | JPEG quality (1–100) |
| `--min-dpi` | | 0 | Only resample images above this DPI |
| `--verbose` | `-v` | false | Show detailed processing info |

### Examples

```bash
# Standard compression — 150 DPI, quality 75
resample-pdf -i scan.pdf -o compressed.pdf

# Print-ready — 300 DPI, high quality
resample-pdf -i document.pdf -o print.pdf -d 300 -q 90

# Web/email — aggressive compression
resample-pdf -i report.pdf -o web.pdf -d 96 -q 60

# Only target extremely high-res images
resample-pdf -i mixed.pdf -o output.pdf -d 200 --min-dpi 400

# Debug mode
resample-pdf -i input.pdf -o output.pdf -v
```

### Web Interface

The browser version provides a drag-and-drop interface with the same options:

- **Target DPI** — resolution threshold for resampling
- **JPEG Quality** — compression quality (1–100)
- **Min DPI Threshold** — skip images below this DPI

All processing happens locally in your browser — no files are uploaded to any server.

## How it works

### Effective DPI

The tool calculates DPI based on **display size**, not pixel count:

```
Effective DPI = Pixels / (Display points ÷ 72)
```

A 3000×2000px image displayed at 10×6.67 inches = 300 DPI.  
The same image at 5×3.33 inches = 600 DPI.

This matters because a tiny thumbnail and a full-page scan might have identical pixel dimensions but vastly different effective resolutions.

### Content stream parsing

PDF images can appear in many places. The tool parses content streams to track transformation matrices and find images in:

- Page content
- Form XObjects (nested graphics)
- Annotation appearances
- Tiling patterns
- Soft mask groups (SMask)

When an image appears multiple times at different sizes, the largest display area is used to preserve quality at the most demanding usage.

### Encoding

| Source | Output |
|--------|--------|
| Opaque images | JPEG (DCTDecode) |
| Images with alpha | FlateDecode RGB + JPEG SMask |
| Fully opaque "alpha" images | Converted to JPEG |

Images are resampled using Lanczos3 interpolation.

## Supported formats

**Color spaces:** DeviceRGB, DeviceGray, DeviceCMYK, ICCBased  
**Input filters:** FlateDecode, DCTDecode (JPEG), JPXDecode (JPEG2000)  
**Output filter:** DCTDecode (JPEG) or FlateDecode (for alpha RGB)

## Limitations

- Indexed and DeviceN color spaces are not supported
- Already-compressed JPEGs may not shrink significantly
- Best results on PDFs with high-DPI raster content (scans, photos, screenshots)

## Disclaimer

This software is provided as-is. Use at your own risk. Always keep backups of your original PDF files before processing. The authors are not responsible for any data loss or corruption.

## License

MIT
