import initWasm, { resample_pdf_with_info } from './pkg/resample_pdf.js';

async function initialize() {
    await initWasm();
    self.postMessage({ type: 'ready' });
}

self.onmessage = function(e) {
    const { type, id } = e.data;
    try {
        switch (type) {
            case 'resample': {
                const pdfBytes = new Uint8Array(e.data.buffer);
                const { targetDpi, quality, minDpi, compressStreams } = e.data;
                const result = resample_pdf_with_info(pdfBytes, targetDpi, quality, minDpi, compressStreams);
                const outputBytes = result.pdf_bytes;
                const reply = {
                    type: 'resampleResult',
                    id,
                    pdfBytes: outputBytes,
                    totalImages: result.total_images,
                    resampledImages: result.resampled_images,
                    skippedImages: result.skipped_images,
                };
                result.free();
                self.postMessage(reply, [outputBytes.buffer]);
                break;
            }
        }
    } catch (err) {
        self.postMessage({ type: 'error', id, message: err.message || String(err) });
    }
};

initialize().catch(err => {
    self.postMessage({ type: 'error', message: 'Failed to initialize WASM: ' + err.message });
});
