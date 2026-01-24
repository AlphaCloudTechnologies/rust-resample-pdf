//! WebAssembly bindings for PDF Image Resampler

use wasm_bindgen::prelude::*;
use crate::{resample_pdf_bytes, extract_pdf_images_info, ResampleOptions};

/// Initialize panic hook for better error messages in browser console
#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// Resample images in a PDF to a target DPI
///
/// # Arguments
/// * `pdf_bytes` - The input PDF file as a byte array
/// * `target_dpi` - Target DPI for images (default: 150)
/// * `quality` - JPEG quality 1-100 (default: 75)
/// * `min_dpi` - Minimum DPI threshold - only resample images above this DPI (default: 0)
///
/// # Returns
/// The resampled PDF as a byte array, or throws an error
#[wasm_bindgen]
pub fn resample_pdf(
    pdf_bytes: &[u8],
    target_dpi: Option<f32>,
    quality: Option<u8>,
    min_dpi: Option<f32>,
) -> Result<Vec<u8>, JsError> {
    let options = ResampleOptions {
        target_dpi: target_dpi.unwrap_or(150.0),
        quality: quality.unwrap_or(75),
        min_dpi: min_dpi.unwrap_or(0.0),
        verbose: false,
    };

    let (output_bytes, _result) = resample_pdf_bytes(pdf_bytes, &options)
        .map_err(|e| JsError::new(&e.to_string()))?;

    Ok(output_bytes)
}

/// Resample images in a PDF with detailed result information
///
/// # Arguments
/// * `pdf_bytes` - The input PDF file as a byte array
/// * `target_dpi` - Target DPI for images (default: 150)
/// * `quality` - JPEG quality 1-100 (default: 75)
/// * `min_dpi` - Minimum DPI threshold - only resample images above this DPI (default: 0)
///
/// # Returns
/// A `ResampleResultJs` object containing the resampled PDF and statistics
#[wasm_bindgen]
pub fn resample_pdf_with_info(
    pdf_bytes: &[u8],
    target_dpi: Option<f32>,
    quality: Option<u8>,
    min_dpi: Option<f32>,
) -> Result<ResampleResultJs, JsError> {
    let options = ResampleOptions {
        target_dpi: target_dpi.unwrap_or(150.0),
        quality: quality.unwrap_or(75),
        min_dpi: min_dpi.unwrap_or(0.0),
        verbose: false,
    };

    // Get image info from the output PDF
    let (output_bytes, result) = resample_pdf_bytes(pdf_bytes, &options)
        .map_err(|e| JsError::new(&e.to_string()))?;

    // Extract image info from the output PDF
    let page_images = extract_pdf_images_info(&output_bytes)
        .map_err(|e| JsError::new(&e.to_string()))?;

    // Convert to JS-friendly format
    let image_info_json = serde_json::to_string(&page_images_to_json(&page_images))
        .unwrap_or_else(|_| "[]".to_string());

    Ok(ResampleResultJs {
        pdf_bytes: output_bytes,
        total_images: result.total_images,
        resampled_images: result.resampled_images,
        skipped_images: result.skipped_images,
        image_info_json,
    })
}

/// Convert page images to a JSON-serializable structure
fn page_images_to_json(pages: &[crate::PageImages]) -> Vec<serde_json::Value> {
    pages.iter().map(|page| {
        serde_json::json!({
            "page": page.page_number,
            "images": page.images.iter().map(|img| {
                serde_json::json!({
                    "objectId": format!("{} {}", img.object_id.0, img.object_id.1),
                    "type": img.image_type,
                    "width": img.width,
                    "height": img.height,
                    "colorSpace": img.color_space,
                    "bpc": img.bits_per_component,
                    "filter": img.filter,
                    "size": img.size_bytes,
                    "dpi": img.effective_dpi
                })
            }).collect::<Vec<_>>()
        })
    }).collect()
}

/// Result of PDF resampling operation with statistics
#[wasm_bindgen]
pub struct ResampleResultJs {
    pdf_bytes: Vec<u8>,
    total_images: usize,
    resampled_images: usize,
    skipped_images: usize,
    image_info_json: String,
}

#[wasm_bindgen]
impl ResampleResultJs {
    /// Get the resampled PDF bytes
    #[wasm_bindgen(getter)]
    pub fn pdf_bytes(&self) -> Vec<u8> {
        self.pdf_bytes.clone()
    }

    /// Get the total number of images found
    #[wasm_bindgen(getter)]
    pub fn total_images(&self) -> usize {
        self.total_images
    }

    /// Get the number of images that were resampled
    #[wasm_bindgen(getter)]
    pub fn resampled_images(&self) -> usize {
        self.resampled_images
    }

    /// Get the number of images that were skipped
    #[wasm_bindgen(getter)]
    pub fn skipped_images(&self) -> usize {
        self.skipped_images
    }

    /// Get detailed image information as JSON string
    #[wasm_bindgen(getter)]
    pub fn image_info_json(&self) -> String {
        self.image_info_json.clone()
    }
}
