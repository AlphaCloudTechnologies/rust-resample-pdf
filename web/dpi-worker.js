import initWasm, { get_pdf_image_info, get_image_data } from './pkg/resample_pdf.js';

let pdfBytes = null;

async function initialize() {
    await initWasm();
    self.postMessage({ type: 'ready' });
}

self.onmessage = function(e) {
    const { type, id } = e.data;
    try {
        switch (type) {
            case 'analyze': {
                pdfBytes = new Uint8Array(e.data.buffer);
                const json = get_pdf_image_info(pdfBytes);
                self.postMessage({ type: 'analyzeResult', id, json });
                break;
            }
            case 'getImage': {
                if (!pdfBytes) throw new Error('No PDF loaded');
                const result = get_image_data(pdfBytes, e.data.objectId);
                const data = result.data;
                const format = result.format;
                const mimeType = result.mime_type;
                result.free();
                self.postMessage(
                    { type: 'imageResult', id, objectId: e.data.objectId, data, format, mimeType },
                    [data.buffer]
                );
                break;
            }
            case 'clear': {
                pdfBytes = null;
                self.postMessage({ type: 'cleared', id });
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
