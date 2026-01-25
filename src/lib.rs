//! PDF Image Resampler Library
//!
//! Core logic for resampling images in PDFs. Shared between CLI and WASM targets.
//!
//! Parses all content streams (pages, Form XObjects, annotations) to extract
//! accurate display dimensions for all images, then resamples them.

#[cfg(target_arch = "wasm32")]
pub mod wasm;

use flate2::read::ZlibDecoder;
use image::{DynamicImage, ImageFormat, RgbImage};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use std::collections::{HashMap, HashSet};
use std::io::Read;

/// Options for PDF resampling
#[derive(Debug, Clone)]
pub struct ResampleOptions {
    /// Target DPI for images (based on display dimensions)
    pub target_dpi: f32,
    /// JPEG quality (1-100, only affects images without alpha)
    pub quality: u8,
    /// Minimum DPI threshold - only resample images above this DPI
    pub min_dpi: f32,
    /// Compress PDF streams (reduces file size)
    pub compress_streams: bool,
    /// Verbose output
    pub verbose: bool,
}

impl Default for ResampleOptions {
    fn default() -> Self {
        Self {
            target_dpi: 150.0,
            quality: 75,
            min_dpi: 0.0,
            compress_streams: true,
            verbose: false,
        }
    }
}

/// Result of PDF resampling operation
#[derive(Debug, Clone)]
pub struct ResampleResult {
    pub total_images: usize,
    pub resampled_images: usize,
    pub skipped_images: usize,
}

/// Information about a single image in the PDF
#[derive(Debug, Clone)]
pub struct ImageInfo {
    /// Object ID (generation, number)
    pub object_id: (u32, u16),
    /// Image type (image or smask)
    pub image_type: String,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Color space
    pub color_space: String,
    /// Bits per component
    pub bits_per_component: u32,
    /// Filter/encoding
    pub filter: String,
    /// Size in bytes
    pub size_bytes: usize,
    /// Effective DPI X (if display info available)
    pub dpi_x: Option<f32>,
    /// Effective DPI Y (if display info available)
    pub dpi_y: Option<f32>,
}

/// Images grouped by page
#[derive(Debug, Clone)]
pub struct PageImages {
    pub page_number: u32,
    pub images: Vec<ImageInfo>,
}

/// Error type for PDF resampling operations
#[derive(Debug)]
pub enum ResampleError {
    InvalidQuality,
    LoadError(String),
    SaveError(String),
    ProcessingError(String),
}

impl std::fmt::Display for ResampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResampleError::InvalidQuality => write!(f, "Quality must be between 1 and 100"),
            ResampleError::LoadError(msg) => write!(f, "Failed to load PDF: {}", msg),
            ResampleError::SaveError(msg) => write!(f, "Failed to save PDF: {}", msg),
            ResampleError::ProcessingError(msg) => write!(f, "Processing error: {}", msg),
        }
    }
}

impl std::error::Error for ResampleError {}

/// Information about an image's display dimensions
#[derive(Debug, Clone)]
pub struct ImageDisplayInfo {
    /// Width in pixels (native image dimensions)
    pub pixel_width: u32,
    /// Height in pixels (native image dimensions)
    pub pixel_height: u32,
    /// Display width in points (72 points = 1 inch)
    pub display_width_points: f32,
    /// Display height in points
    pub display_height_points: f32,
}

impl ImageDisplayInfo {
    /// Calculate the effective DPI based on display dimensions
    pub fn effective_dpi_x(&self) -> f32 {
        let display_inches = self.display_width_points / 72.0;
        if display_inches > 0.0 {
            self.pixel_width as f32 / display_inches
        } else {
            0.0
        }
    }

    pub fn effective_dpi_y(&self) -> f32 {
        let display_inches = self.display_height_points / 72.0;
        if display_inches > 0.0 {
            self.pixel_height as f32 / display_inches
        } else {
            0.0
        }
    }

    pub fn max_effective_dpi(&self) -> f32 {
        self.effective_dpi_x().max(self.effective_dpi_y())
    }

    /// Calculate target pixel dimensions for a given DPI
    pub fn target_pixels_for_dpi(&self, target_dpi: f32) -> (u32, u32) {
        let display_width_inches = self.display_width_points / 72.0;
        let display_height_inches = self.display_height_points / 72.0;

        let target_width = (display_width_inches * target_dpi).round() as u32;
        let target_height = (display_height_inches * target_dpi).round() as u32;

        (target_width.max(1), target_height.max(1))
    }
}

/// 2D transformation matrix [a, b, c, d, e, f]
/// Represents: | a b 0 |
///             | c d 0 |
///             | e f 1 |
#[derive(Debug, Clone, Copy)]
struct Matrix {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Matrix {
    fn identity() -> Self {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Concatenate another matrix: self * other
    fn concat(&self, other: &Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// Get the scaling factors (approximate display size)
    fn scale_x(&self) -> f32 {
        (self.a * self.a + self.b * self.b).sqrt()
    }

    fn scale_y(&self) -> f32 {
        (self.c * self.c + self.d * self.d).sqrt()
    }
}

/// Decompress a stream's content
fn decompress_stream(stream: &Stream) -> Vec<u8> {
    let filter = stream.dict.get(b"Filter").ok().and_then(|f| match f {
        Object::Name(n) => Some(vec![String::from_utf8_lossy(n).to_string()]),
        Object::Array(arr) => Some(
            arr.iter()
                .filter_map(|f| match f {
                    Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    });

    let mut data = stream.content.clone();

    if let Some(filters) = filter {
        for filter_name in filters {
            match filter_name.as_str() {
                "FlateDecode" => {
                    let mut decoder = ZlibDecoder::new(&data[..]);
                    let mut decoded = Vec::new();
                    if decoder.read_to_end(&mut decoded).is_ok() {
                        data = decoded;
                    } else {
                        return stream.content.clone();
                    }
                }
                _ => {
                    // Unknown filter, return as-is
                    return data;
                }
            }
        }
    }

    data
}

/// Parse a number from a token
fn parse_number(token: &str) -> Option<f32> {
    token.parse::<f32>().ok()
}

/// Context for scanning content streams
struct ContentScanner<'a> {
    doc: &'a Document,
    /// Map from image object ID to list of display dimensions (image may appear multiple times)
    display_info: HashMap<ObjectId, Vec<(f32, f32)>>,
    /// Image dimensions cache (object ID -> pixel dimensions)
    image_dims: HashMap<ObjectId, (u32, u32)>,
    /// Form XObjects that have been scanned (to avoid infinite loops)
    scanned_forms: HashSet<ObjectId>,
    verbose: bool,
    log_callback: Option<Box<dyn Fn(&str) + 'a>>,
}

impl<'a> ContentScanner<'a> {
    fn new(doc: &'a Document, verbose: bool) -> Self {
        let mut scanner = ContentScanner {
            doc,
            display_info: HashMap::new(),
            image_dims: HashMap::new(),
            scanned_forms: HashSet::new(),
            verbose,
            log_callback: None,
        };

        // Pre-cache all image dimensions
        scanner.cache_image_dimensions();
        scanner
    }

    fn log(&self, msg: &str) {
        if self.verbose {
            if let Some(ref cb) = self.log_callback {
                cb(msg);
            } else {
                #[cfg(not(target_arch = "wasm32"))]
                println!("{}", msg);
            }
        }
    }

    /// Cache dimensions of all Image XObjects
    fn cache_image_dimensions(&mut self) {
        for (id, object) in self.doc.objects.iter() {
            if let Object::Stream(stream) = object {
                let subtype = stream.dict.get(b"Subtype").ok().and_then(|s| match s {
                    Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
                    _ => None,
                });

                if subtype.as_deref() == Some("Image") {
                    let width = stream
                        .dict
                        .get(b"Width")
                        .ok()
                        .and_then(|w| match w {
                            Object::Integer(n) => Some(*n as u32),
                            _ => None,
                        })
                        .unwrap_or(0);

                    let height = stream
                        .dict
                        .get(b"Height")
                        .ok()
                        .and_then(|h| match h {
                            Object::Integer(n) => Some(*n as u32),
                            _ => None,
                        })
                        .unwrap_or(0);

                    if width > 0 && height > 0 {
                        self.image_dims.insert(*id, (width, height));
                    }
                }
            }
        }
    }

    /// Resolve a reference to get the actual object
    fn resolve<'b>(&'b self, obj: &'b Object) -> Option<&'b Object> {
        match obj {
            Object::Reference(id) => self.doc.get_object(*id).ok(),
            _ => Some(obj),
        }
    }

    /// Get XObject dictionary from resources
    fn get_xobjects_from_resources(&self, resources: &Object) -> HashMap<String, ObjectId> {
        let mut result = HashMap::new();

        let res_dict = match self.resolve(resources) {
            Some(Object::Dictionary(d)) => Some(d),
            _ => None,
        };

        if let Some(res_dict) = res_dict {
            if let Ok(xobjects) = res_dict.get(b"XObject") {
                let xobj_dict = match self.resolve(xobjects) {
                    Some(Object::Dictionary(d)) => Some(d),
                    _ => None,
                };

                if let Some(xobj_dict) = xobj_dict {
                    for (name, value) in xobj_dict.iter() {
                        let name_str = String::from_utf8_lossy(name).to_string();
                        if let Object::Reference(obj_id) = value {
                            result.insert(name_str, *obj_id);
                        }
                    }
                }
            }
        }

        result
    }

    /// Get ExtGState dictionary from resources (name -> object ID)
    fn get_extgstates_from_resources(&self, resources: &Object) -> HashMap<String, ObjectId> {
        let mut result = HashMap::new();

        let res_dict = match self.resolve(resources) {
            Some(Object::Dictionary(d)) => Some(d),
            _ => None,
        };

        if let Some(res_dict) = res_dict {
            if let Ok(extgstate) = res_dict.get(b"ExtGState") {
                let gs_dict = match self.resolve(extgstate) {
                    Some(Object::Dictionary(d)) => Some(d),
                    _ => None,
                };

                if let Some(gs_dict) = gs_dict {
                    for (name, value) in gs_dict.iter() {
                        let name_str = String::from_utf8_lossy(name).to_string();
                        if let Object::Reference(obj_id) = value {
                            result.insert(name_str, *obj_id);
                        }
                    }
                }
            }
        }

        result
    }

    /// Get SMask Form XObject ID from an ExtGState object
    fn get_smask_form_from_extgstate(&self, gs_id: ObjectId) -> Option<ObjectId> {
        let gs_obj = self.doc.get_object(gs_id).ok()?;
        let gs_dict = match gs_obj {
            Object::Dictionary(d) => d,
            _ => return None,
        };

        let smask = gs_dict.get(b"SMask").ok()?;

        // SMask can be a dictionary or /None
        match smask {
            Object::Dictionary(dict) => {
                // SMask dictionary with /G entry pointing to Form XObject
                if let Ok(g) = dict.get(b"G") {
                    if let Object::Reference(form_id) = g {
                        return Some(*form_id);
                    }
                }
            }
            Object::Reference(id) => {
                // Reference to SMask dictionary
                if let Ok(Object::Dictionary(dict)) = self.doc.get_object(*id) {
                    if let Ok(g) = dict.get(b"G") {
                        if let Object::Reference(form_id) = g {
                            return Some(*form_id);
                        }
                    }
                }
            }
            _ => {}
        }

        None
    }

    /// Get Pattern dictionary from resources and collect Form XObjects from tiling patterns
    fn get_pattern_forms_from_resources(&self, resources: &Object) -> Vec<ObjectId> {
        let mut result = Vec::new();

        let res_dict = match self.resolve(resources) {
            Some(Object::Dictionary(d)) => Some(d),
            _ => None,
        };

        if let Some(res_dict) = res_dict {
            if let Ok(patterns) = res_dict.get(b"Pattern") {
                let pat_dict = match self.resolve(patterns) {
                    Some(Object::Dictionary(d)) => Some(d),
                    _ => None,
                };

                if let Some(pat_dict) = pat_dict {
                    for (_, value) in pat_dict.iter() {
                        // Each entry is a reference to a Pattern
                        if let Object::Reference(pat_id) = value {
                            // Check if it's a tiling pattern (has content stream)
                            if let Ok(Object::Stream(stream)) = self.doc.get_object(*pat_id) {
                                let pattern_type =
                                    stream.dict.get(b"PatternType").ok().and_then(|p| match p {
                                        Object::Integer(n) => Some(*n),
                                        _ => None,
                                    });
                                // PatternType 1 = Tiling pattern (has content stream)
                                if pattern_type == Some(1) {
                                    result.push(*pat_id);
                                }
                            }
                        }
                    }
                }
            }
        }

        result
    }

    /// Parse and scan a content stream
    fn scan_content_stream(&mut self, content: &[u8], resources: &Object, initial_matrix: Matrix) {
        let xobjects = self.get_xobjects_from_resources(resources);

        // Get ExtGState dictionary for SMask lookups
        let extgstates = self.get_extgstates_from_resources(resources);

        // Also scan tiling patterns (these are used with pattern color space)
        let pattern_forms = self.get_pattern_forms_from_resources(resources);
        for pattern_id in pattern_forms {
            self.scan_tiling_pattern(pattern_id, initial_matrix);
        }

        let content_str = String::from_utf8_lossy(content);

        // Graphics state stack
        let mut matrix_stack: Vec<Matrix> = vec![initial_matrix];

        // Tokenize (simplified - doesn't handle all PDF syntax but works for most cases)
        let mut tokens: Vec<String> = Vec::new();
        let mut in_string = false;
        let mut paren_depth = 0;
        let mut current_token = String::new();

        for ch in content_str.chars() {
            if in_string {
                current_token.push(ch);
                if ch == '(' {
                    paren_depth += 1;
                } else if ch == ')' {
                    paren_depth -= 1;
                    if paren_depth == 0 {
                        in_string = false;
                        tokens.push(current_token.clone());
                        current_token.clear();
                    }
                }
            } else {
                match ch {
                    '(' => {
                        if !current_token.is_empty() {
                            tokens.push(current_token.clone());
                            current_token.clear();
                        }
                        in_string = true;
                        paren_depth = 1;
                        current_token.push(ch);
                    }
                    ' ' | '\t' | '\n' | '\r' => {
                        if !current_token.is_empty() {
                            tokens.push(current_token.clone());
                            current_token.clear();
                        }
                    }
                    '[' | ']' => {
                        if !current_token.is_empty() {
                            tokens.push(current_token.clone());
                            current_token.clear();
                        }
                        tokens.push(ch.to_string());
                    }
                    _ => {
                        current_token.push(ch);
                    }
                }
            }
        }
        if !current_token.is_empty() {
            tokens.push(current_token);
        }

        // Process tokens
        let mut i = 0;
        while i < tokens.len() {
            let token = &tokens[i];

            match token.as_str() {
                "q" => {
                    // Save graphics state
                    if let Some(current) = matrix_stack.last() {
                        matrix_stack.push(*current);
                    }
                }
                "Q" => {
                    // Restore graphics state
                    if matrix_stack.len() > 1 {
                        matrix_stack.pop();
                    }
                }
                "cm" => {
                    // Concatenate matrix: a b c d e f cm
                    if i >= 6 {
                        let a = parse_number(&tokens[i - 6]);
                        let b = parse_number(&tokens[i - 5]);
                        let c = parse_number(&tokens[i - 4]);
                        let d = parse_number(&tokens[i - 3]);
                        let e = parse_number(&tokens[i - 2]);
                        let f = parse_number(&tokens[i - 1]);

                        if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) =
                            (a, b, c, d, e, f)
                        {
                            let new_matrix = Matrix { a, b, c, d, e, f };
                            if let Some(current) = matrix_stack.last_mut() {
                                *current = current.concat(&new_matrix);
                            }
                        }
                    }
                }
                "gs" => {
                    // Set graphics state: /Name gs
                    if i >= 1 {
                        let name = tokens[i - 1].trim_start_matches('/');
                        if let Some(&gs_id) = extgstates.get(name) {
                            let current_matrix =
                                matrix_stack.last().copied().unwrap_or(Matrix::identity());

                            // Check if this ExtGState has an SMask with a Form XObject
                            if let Some(form_id) = self.get_smask_form_from_extgstate(gs_id) {
                                // Scan the SMask Form with the current transformation
                                self.scan_form_xobject(form_id, current_matrix);
                            }
                        }
                    }
                }
                "Do" => {
                    // XObject invocation: /Name Do
                    if i >= 1 {
                        let name = tokens[i - 1].trim_start_matches('/');
                        if let Some(&obj_id) = xobjects.get(name) {
                            let current_matrix =
                                matrix_stack.last().copied().unwrap_or(Matrix::identity());

                            // Check if it's an image or form
                            if let Ok(Object::Stream(stream)) = self.doc.get_object(obj_id) {
                                let subtype =
                                    stream.dict.get(b"Subtype").ok().and_then(|s| match s {
                                        Object::Name(n) => {
                                            Some(String::from_utf8_lossy(n).to_string())
                                        }
                                        _ => None,
                                    });

                                match subtype.as_deref() {
                                    Some("Image") => {
                                        // Record display dimensions for this image
                                        let display_w = current_matrix.scale_x();
                                        let display_h = current_matrix.scale_y();

                                        if display_w > 0.0 && display_h > 0.0 {
                                            self.display_info
                                                .entry(obj_id)
                                                .or_default()
                                                .push((display_w, display_h));
                                        }
                                    }
                                    Some("Form") => {
                                        // Recursively scan Form XObject
                                        self.scan_form_xobject(obj_id, current_matrix);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// Scan a Form XObject's content stream
    fn scan_form_xobject(&mut self, form_id: ObjectId, parent_matrix: Matrix) {
        // Avoid infinite recursion
        if self.scanned_forms.contains(&form_id) {
            return;
        }
        self.scanned_forms.insert(form_id);

        let stream = match self.doc.get_object(form_id) {
            Ok(Object::Stream(s)) => s.clone(),
            _ => return,
        };

        // Get Form's transformation matrix (if any)
        let form_matrix = self.parse_matrix_from_dict(&stream.dict);

        // Combined matrix = parent * form
        let combined_matrix = parent_matrix.concat(&form_matrix);

        // Get resources
        let resources = stream
            .dict
            .get(b"Resources")
            .cloned()
            .unwrap_or(Object::Null);

        // Decompress and scan content
        let content = decompress_stream(&stream);
        self.scan_content_stream(&content, &resources, combined_matrix);
    }

    /// Scan a tiling pattern's content stream
    fn scan_tiling_pattern(&mut self, pattern_id: ObjectId, parent_matrix: Matrix) {
        // Avoid infinite recursion (patterns can be in scanned_forms too)
        if self.scanned_forms.contains(&pattern_id) {
            return;
        }
        self.scanned_forms.insert(pattern_id);

        let stream = match self.doc.get_object(pattern_id) {
            Ok(Object::Stream(s)) => s.clone(),
            _ => return,
        };

        // Get pattern's transformation matrix
        let pattern_matrix = self.parse_matrix_from_dict(&stream.dict);

        // Combined matrix = parent * pattern
        let combined_matrix = parent_matrix.concat(&pattern_matrix);

        // Get resources
        let resources = stream
            .dict
            .get(b"Resources")
            .cloned()
            .unwrap_or(Object::Null);

        // Decompress and scan content
        let content = decompress_stream(&stream);
        self.scan_content_stream(&content, &resources, combined_matrix);
    }

    /// Parse a transformation matrix from a dictionary's /Matrix entry
    fn parse_matrix_from_dict(&self, dict: &Dictionary) -> Matrix {
        dict.get(b"Matrix")
            .ok()
            .and_then(|m| match m {
                Object::Array(arr) if arr.len() >= 6 => {
                    let get_num = |obj: &Object| -> Option<f32> {
                        match obj {
                            Object::Integer(n) => Some(*n as f32),
                            Object::Real(n) => Some(*n),
                            _ => None,
                        }
                    };
                    Some(Matrix {
                        a: get_num(&arr[0])?,
                        b: get_num(&arr[1])?,
                        c: get_num(&arr[2])?,
                        d: get_num(&arr[3])?,
                        e: get_num(&arr[4])?,
                        f: get_num(&arr[5])?,
                    })
                }
                _ => None,
            })
            .unwrap_or(Matrix::identity())
    }

    /// Scan all pages in the document
    fn scan_all_pages(&mut self) {
        // Get page tree
        let pages = match self.doc.get_pages() {
            pages if !pages.is_empty() => pages,
            _ => return,
        };

        for (page_num, &page_id) in pages.iter() {
            self.log(&format!("[Scanner] Scanning page {}...", page_num));

            let page_dict = match self.doc.get_object(page_id) {
                Ok(Object::Dictionary(d)) => d.clone(),
                _ => continue,
            };

            // Get page resources
            let resources = self.get_page_resources(&page_dict, page_id);

            // Get page contents
            let contents = page_dict.get(b"Contents").ok();

            if let Some(contents) = contents {
                let content_data = self.get_content_data(contents);
                self.scan_content_stream(&content_data, &resources, Matrix::identity());
            }

            // Scan annotations on this page
            self.scan_page_annotations(&page_dict);
        }
    }

    /// Get resources for a page, checking parent pages if needed
    fn get_page_resources(&self, page_dict: &Dictionary, page_id: ObjectId) -> Object {
        // First try to get resources directly from the page
        if let Ok(resources) = page_dict.get(b"Resources") {
            return resources.clone();
        }

        // Otherwise, look in parent (for inherited resources)
        if let Ok(Object::Reference(parent_id)) = page_dict.get(b"Parent") {
            if let Ok(Object::Dictionary(parent_dict)) = self.doc.get_object(*parent_id) {
                if let Ok(resources) = parent_dict.get(b"Resources") {
                    return resources.clone();
                }
            }
        }

        // Try to get from document catalog
        if let Ok(catalog) = self.doc.catalog() {
            if let Ok(pages_ref) = catalog.get(b"Pages") {
                if let Object::Reference(pages_id) = pages_ref {
                    if let Ok(Object::Dictionary(pages_dict)) = self.doc.get_object(*pages_id) {
                        if let Ok(resources) = pages_dict.get(b"Resources") {
                            return resources.clone();
                        }
                    }
                }
            }
        }

        let _ = page_id; // suppress unused warning
        Object::Null
    }

    /// Get content data from a Contents entry (may be stream or array of streams)
    fn get_content_data(&self, contents: &Object) -> Vec<u8> {
        match contents {
            Object::Reference(id) => {
                if let Ok(obj) = self.doc.get_object(*id) {
                    self.get_content_data(obj)
                } else {
                    Vec::new()
                }
            }
            Object::Stream(stream) => decompress_stream(stream),
            Object::Array(arr) => {
                let mut combined = Vec::new();
                for item in arr {
                    let data = self.get_content_data(item);
                    combined.extend(data);
                    combined.push(b'\n');
                }
                combined
            }
            _ => Vec::new(),
        }
    }

    /// Scan annotations on a page
    fn scan_page_annotations(&mut self, page_dict: &Dictionary) {
        let annots = match page_dict.get(b"Annots").ok() {
            Some(a) => a,
            None => return,
        };

        let annot_array = match self.resolve(annots) {
            Some(Object::Array(arr)) => arr.clone(),
            _ => return,
        };

        for annot_ref in annot_array {
            if let Object::Reference(annot_id) = annot_ref {
                self.scan_annotation(annot_id);
            }
        }
    }

    /// Scan an annotation's appearance streams
    fn scan_annotation(&mut self, annot_id: ObjectId) {
        let annot_dict = match self.doc.get_object(annot_id) {
            Ok(Object::Dictionary(d)) => d.clone(),
            _ => return,
        };

        // Get appearance dictionary (AP)
        let ap = match annot_dict.get(b"AP").ok() {
            Some(ap) => ap,
            None => return,
        };

        let ap_dict = match self.resolve(ap) {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => return,
        };

        // Scan Normal (N), Rollover (R), and Down (D) appearances
        for key in [b"N".as_slice(), b"R".as_slice(), b"D".as_slice()] {
            if let Ok(appearance) = ap_dict.get(key) {
                self.scan_appearance_entry(appearance);
            }
        }
    }

    /// Scan an appearance entry (may be a stream or dictionary of streams)
    fn scan_appearance_entry(&mut self, appearance: &Object) {
        // First, collect any object IDs we need to scan
        let mut ids_to_scan: Vec<ObjectId> = Vec::new();

        match appearance {
            Object::Reference(id) => {
                // Check what the reference points to
                if let Ok(obj) = self.doc.get_object(*id) {
                    match obj {
                        Object::Stream(_) => {
                            // Single appearance stream - treat as Form XObject
                            ids_to_scan.push(*id);
                        }
                        Object::Dictionary(dict) => {
                            // Dictionary of appearance states
                            for (_, value) in dict.iter() {
                                if let Object::Reference(ref_id) = value {
                                    ids_to_scan.push(*ref_id);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Object::Dictionary(dict) => {
                // Inline dictionary of appearance states
                for (_, value) in dict.iter() {
                    if let Object::Reference(ref_id) = value {
                        ids_to_scan.push(*ref_id);
                    }
                }
            }
            _ => {}
        }

        // Now scan all collected IDs
        for id in ids_to_scan {
            self.scan_form_xobject(id, Matrix::identity());
        }
    }

    /// Get the final display info map (object ID -> best display info)
    fn get_display_info_map(&self) -> HashMap<ObjectId, ImageDisplayInfo> {
        let mut result = HashMap::new();

        for (obj_id, display_dims) in &self.display_info {
            if let Some(&(pixel_w, pixel_h)) = self.image_dims.get(obj_id) {
                // Use the largest display size (most conservative - preserves most detail)
                let (display_w, display_h) = display_dims
                    .iter()
                    .max_by(|(w1, h1), (w2, h2)| {
                        let area1 = w1 * h1;
                        let area2 = w2 * h2;
                        area1.partial_cmp(&area2).unwrap()
                    })
                    .copied()
                    .unwrap_or((pixel_w as f32, pixel_h as f32));

                result.insert(
                    *obj_id,
                    ImageDisplayInfo {
                        pixel_width: pixel_w,
                        pixel_height: pixel_h,
                        display_width_points: display_w,
                        display_height_points: display_h,
                    },
                );
            }
        }

        result
    }
}

/// Decode an SMask stream (grayscale alpha channel)
fn decode_smask_stream(stream: &Stream, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let content = &stream.content;
    let filter = stream.dict.get(b"Filter").ok().and_then(|f| match f {
        Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
        Object::Array(arr) => arr.first().and_then(|f| match f {
            Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
            _ => None,
        }),
        _ => None,
    });

    let decoded_data = match filter.as_deref() {
        Some("FlateDecode") => {
            let mut decoder = ZlibDecoder::new(&content[..]);
            let mut decoded = Vec::new();
            decoder
                .read_to_end(&mut decoded)
                .map_err(|e| e.to_string())?;
            decoded
        }
        None => content.clone(),
        Some(other) => {
            return Err(format!("Unsupported SMask filter: {}", other));
        }
    };

    let expected_size = (width * height) as usize;
    if decoded_data.len() >= expected_size {
        Ok(decoded_data[..expected_size].to_vec())
    } else {
        Err(format!(
            "SMask data size mismatch: got {} expected {}",
            decoded_data.len(),
            expected_size
        ))
    }
}

/// Decode a PDF image stream into raw pixel data
fn decode_image_stream(
    stream: &Stream,
    width: u32,
    height: u32,
    color_space: &str,
    bits_per_component: u32,
) -> Result<DynamicImage, String> {
    let content = &stream.content;
    let filter = stream.dict.get(b"Filter").ok().and_then(|f| match f {
        Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
        Object::Array(arr) => arr.first().and_then(|f| match f {
            Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
            _ => None,
        }),
        _ => None,
    });

    let decoded_data = match filter.as_deref() {
        Some("FlateDecode") => {
            let mut decoder = ZlibDecoder::new(&content[..]);
            let mut decoded = Vec::new();
            decoder
                .read_to_end(&mut decoded)
                .map_err(|e| e.to_string())?;
            decoded
        }
        Some("DCTDecode") => {
            // JPEG data - decode using image crate
            let img = image::load_from_memory_with_format(content, ImageFormat::Jpeg)
                .map_err(|e| format!("Failed to decode JPEG image: {}", e))?;
            return Ok(img);
        }
        Some("JPXDecode") => {
            // JPEG2000 - try to decode using image crate
            let img = image::load_from_memory(content)
                .map_err(|e| format!("Failed to decode JPEG2000 image: {}", e))?;
            return Ok(img);
        }
        None => content.clone(),
        Some(other) => {
            return Err(format!("Unsupported filter: {}", other));
        }
    };

    // Convert raw pixel data to DynamicImage based on color space
    match color_space {
        "DeviceRGB" | "RGB" => {
            let expected_size = (width * height * 3) as usize;
            if bits_per_component == 8 && decoded_data.len() >= expected_size {
                let img = RgbImage::from_raw(width, height, decoded_data[..expected_size].to_vec())
                    .ok_or("Failed to create RGB image from raw data")?;
                Ok(DynamicImage::ImageRgb8(img))
            } else {
                Err(format!(
                    "Unsupported RGB format: {} bits, {} bytes (expected {})",
                    bits_per_component,
                    decoded_data.len(),
                    expected_size
                ))
            }
        }
        "DeviceGray" | "Gray" => {
            let expected_size = (width * height) as usize;
            if bits_per_component == 8 && decoded_data.len() >= expected_size {
                let img = image::GrayImage::from_raw(
                    width,
                    height,
                    decoded_data[..expected_size].to_vec(),
                )
                .ok_or("Failed to create grayscale image from raw data")?;
                Ok(DynamicImage::ImageLuma8(img))
            } else {
                Err(format!(
                    "Unsupported grayscale format: {} bits, {} bytes (expected {})",
                    bits_per_component,
                    decoded_data.len(),
                    expected_size
                ))
            }
        }
        "DeviceCMYK" | "CMYK" => {
            // Convert CMYK to RGB
            let expected_size = (width * height * 4) as usize;
            if bits_per_component == 8 && decoded_data.len() >= expected_size {
                let mut rgb_data = Vec::with_capacity((width * height * 3) as usize);
                for chunk in decoded_data[..expected_size].chunks(4) {
                    let c = chunk[0] as f32 / 255.0;
                    let m = chunk[1] as f32 / 255.0;
                    let y = chunk[2] as f32 / 255.0;
                    let k = chunk[3] as f32 / 255.0;

                    let r = ((1.0 - c) * (1.0 - k) * 255.0) as u8;
                    let g = ((1.0 - m) * (1.0 - k) * 255.0) as u8;
                    let b = ((1.0 - y) * (1.0 - k) * 255.0) as u8;

                    rgb_data.push(r);
                    rgb_data.push(g);
                    rgb_data.push(b);
                }
                let img = RgbImage::from_raw(width, height, rgb_data)
                    .ok_or("Failed to create RGB image from CMYK data")?;
                Ok(DynamicImage::ImageRgb8(img))
            } else {
                Err(format!(
                    "Unsupported CMYK format: {} bits, {} bytes (expected {})",
                    bits_per_component,
                    decoded_data.len(),
                    expected_size
                ))
            }
        }
        "ICCBased" => {
            // Try to guess based on data size
            let pixels = (width * height) as usize;
            if decoded_data.len() >= pixels * 3 && bits_per_component == 8 {
                let img = RgbImage::from_raw(width, height, decoded_data[..pixels * 3].to_vec())
                    .ok_or("Failed to create RGB image from ICCBased data")?;
                Ok(DynamicImage::ImageRgb8(img))
            } else if decoded_data.len() >= pixels && bits_per_component == 8 {
                let img = image::GrayImage::from_raw(width, height, decoded_data[..pixels].to_vec())
                    .ok_or("Failed to create grayscale image from ICCBased data")?;
                Ok(DynamicImage::ImageLuma8(img))
            } else {
                Err("Could not determine ICCBased color space format".to_string())
            }
        }
        _ => Err(format!("Unsupported color space: {}", color_space)),
    }
}

/// Encode an image as JPEG and create a PDF stream
fn encode_as_jpeg_stream(img: &DynamicImage, quality: u8) -> Result<(Stream, u32, u32), String> {
    let rgb = img.to_rgb8();
    let (width, height) = rgb.dimensions();

    let mut jpeg_bytes = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut jpeg_bytes, quality);
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0);
    encoder
        .encode(
            rgb.as_raw(),
            width as u16,
            height as u16,
            jpeg_encoder::ColorType::Rgb,
        )
        .map_err(|e| format!("Failed to encode JPEG: {}", e))?;

    let mut dict = lopdf::Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("Width", Object::Integer(width as i64));
    dict.set("Height", Object::Integer(height as i64));
    dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    dict.set("BitsPerComponent", Object::Integer(8));
    dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
    dict.set("Length", Object::Integer(jpeg_bytes.len() as i64));

    Ok((Stream::new(dict, jpeg_bytes), width, height))
}

/// Create an SMask stream for the alpha channel using JPEG compression
fn create_smask_stream(alpha_data: &[u8], width: u32, height: u32, quality: u8) -> Result<Stream, String> {
    let mut jpeg_bytes = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut jpeg_bytes, quality);
    encoder
        .encode(
            alpha_data,
            width as u16,
            height as u16,
            jpeg_encoder::ColorType::Luma,
        )
        .map_err(|e| format!("Failed to encode SMask as JPEG: {}", e))?;

    let mut dict = lopdf::Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("Width", Object::Integer(width as i64));
    dict.set("Height", Object::Integer(height as i64));
    dict.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
    dict.set("BitsPerComponent", Object::Integer(8));
    dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
    dict.set("Length", Object::Integer(jpeg_bytes.len() as i64));

    Ok(Stream::new(dict, jpeg_bytes))
}

/// Encode an image with alpha
fn encode_with_alpha_stream(
    img: &DynamicImage,
    quality: u8,
) -> Result<(Stream, Option<Stream>, u32, u32), String> {
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let raw_data = rgba.into_raw();

    // Separate RGB and Alpha channels
    let mut rgb_data = Vec::with_capacity((width * height * 3) as usize);
    let mut alpha_data = Vec::with_capacity((width * height) as usize);

    for chunk in raw_data.chunks(4) {
        rgb_data.push(chunk[0]);
        rgb_data.push(chunk[1]);
        rgb_data.push(chunk[2]);
        alpha_data.push(chunk[3]);
    }

    // Compress RGB with FlateDecode
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    std::io::Write::write_all(&mut encoder, &rgb_data)
        .map_err(|e| format!("Failed to compress RGB data: {}", e))?;
    let compressed_rgb = encoder
        .finish()
        .map_err(|e| format!("Failed to finish compression: {}", e))?;

    let mut dict = lopdf::Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("Width", Object::Integer(width as i64));
    dict.set("Height", Object::Integer(height as i64));
    dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    dict.set("BitsPerComponent", Object::Integer(8));
    dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
    dict.set("Length", Object::Integer(compressed_rgb.len() as i64));

    let main_stream = Stream::new(dict, compressed_rgb);
    let smask_stream = create_smask_stream(&alpha_data, width, height, quality)?;

    Ok((main_stream, Some(smask_stream), width, height))
}

/// Get color space name from PDF object
fn get_color_space_name(obj: &Object, doc: &Document) -> String {
    match obj {
        Object::Name(name) => String::from_utf8_lossy(name).to_string(),
        Object::Array(arr) => {
            if let Some(Object::Name(name)) = arr.first() {
                String::from_utf8_lossy(name).to_string()
            } else {
                "Unknown".to_string()
            }
        }
        Object::Reference(id) => {
            if let Ok(resolved) = doc.get_object(*id) {
                get_color_space_name(resolved, doc)
            } else {
                "Unknown".to_string()
            }
        }
        _ => "Unknown".to_string(),
    }
}

/// Check if an image has meaningful alpha
fn has_alpha(img: &DynamicImage) -> bool {
    match img {
        DynamicImage::ImageRgba8(rgba) => {
            let sample_rate = std::cmp::max(1, rgba.pixels().len() / 10000);
            rgba.pixels().step_by(sample_rate).any(|p| p.0[3] < 255)
        }
        DynamicImage::ImageLumaA8(la) => {
            let sample_rate = std::cmp::max(1, la.pixels().len() / 10000);
            la.pixels().step_by(sample_rate).any(|p| p.0[1] < 255)
        }
        _ => false,
    }
}

/// Resample an image to target dimensions
fn resample_image(img: &DynamicImage, target_width: u32, target_height: u32) -> DynamicImage {
    img.resize_exact(
        target_width,
        target_height,
        image::imageops::FilterType::Lanczos3,
    )
}

/// Process images in PDF document (in-memory version)
fn process_images_in_doc(
    doc: &mut Document,
    display_info_map: &HashMap<ObjectId, ImageDisplayInfo>,
    options: &ResampleOptions,
    log: impl Fn(&str),
) -> Result<ResampleResult, String> {
    let mut total_images = 0;
    let mut resampled_images = 0;
    let mut skipped_images = 0;

    // Collect all image XObjects
    let mut image_objects: Vec<ObjectId> = Vec::new();

    for (id, object) in doc.objects.iter() {
        if let Object::Stream(stream) = object {
            let subtype = stream.dict.get(b"Subtype").ok().and_then(|s| match s {
                Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
                _ => None,
            });

            if subtype.as_deref() == Some("Image") {
                image_objects.push(*id);
            }
        }
    }

    if options.verbose {
        log(&format!("[Process] Found {} image XObjects", image_objects.len()));
    }

    // Process each image
    for object_id in image_objects {
        let stream = match doc.get_object(object_id) {
            Ok(Object::Stream(s)) => s.clone(),
            _ => continue,
        };

        total_images += 1;

        // Get image dimensions
        let width = stream
            .dict
            .get(b"Width")
            .ok()
            .and_then(|w| match w {
                Object::Integer(n) => Some(*n as u32),
                _ => None,
            })
            .unwrap_or(0);

        let height = stream
            .dict
            .get(b"Height")
            .ok()
            .and_then(|h| match h {
                Object::Integer(n) => Some(*n as u32),
                _ => None,
            })
            .unwrap_or(0);

        if width == 0 || height == 0 {
            if options.verbose {
                log(&format!("[Process] Skipping {:?}: invalid dimensions", object_id));
            }
            skipped_images += 1;
            continue;
        }

        // Check current encoding
        let current_filter = stream.dict.get(b"Filter").ok().and_then(|f| match f {
            Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
            Object::Array(arr) => arr.first().and_then(|f| match f {
                Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
                _ => None,
            }),
            _ => None,
        });
        let is_already_jpeg = current_filter.as_deref() == Some("DCTDecode");

        // Look up display info
        let display_info = display_info_map.get(&object_id).cloned().unwrap_or_else(|| {
            if options.verbose {
                log(&format!(
                    "[Process] Image {:?} ({}x{}): No display info found, using pixel dims",
                    object_id, width, height
                ));
            }
            // Fall back to assuming 72 DPI (1 pixel = 1 point)
            ImageDisplayInfo {
                pixel_width: width,
                pixel_height: height,
                display_width_points: width as f32,
                display_height_points: height as f32,
            }
        });

        let current_dpi = display_info.max_effective_dpi();

        if options.verbose {
            log(&format!(
                "[Process] Image {:?}: {}x{} px, {:.1}x{:.1} pt, {:.1} DPI ({})",
                object_id,
                width,
                height,
                display_info.display_width_points,
                display_info.display_height_points,
                current_dpi,
                current_filter.as_deref().unwrap_or("raw")
            ));
        }

        // Check if resampling is needed
        let needs_resampling = current_dpi > options.target_dpi + 1.0 && current_dpi > options.min_dpi;

        // Calculate target dimensions
        let (target_width, target_height) = if needs_resampling {
            display_info.target_pixels_for_dpi(options.target_dpi)
        } else {
            (width, height)
        };

        // Skip if already JPEG and no resampling needed
        if !needs_resampling && is_already_jpeg {
            if options.verbose {
                log("  Skipping: Already JPEG at target DPI");
            }
            skipped_images += 1;
            continue;
        }

        // Skip if resampling would make image larger
        if needs_resampling && target_width >= width && target_height >= height {
            if options.verbose {
                log("  Skipping: Target dimensions not smaller");
            }
            skipped_images += 1;
            continue;
        }

        // Get color space and bits per component
        let color_space = stream
            .dict
            .get(b"ColorSpace")
            .ok()
            .map(|cs| get_color_space_name(cs, doc))
            .unwrap_or_else(|| "DeviceRGB".to_string());

        let bits_per_component = stream
            .dict
            .get(b"BitsPerComponent")
            .ok()
            .and_then(|b| match b {
                Object::Integer(n) => Some(*n as u32),
                _ => None,
            })
            .unwrap_or(8);

        // Check for SMask
        let smask_id = stream.dict.get(b"SMask").ok().and_then(|s| match s {
            Object::Reference(id) => Some(*id),
            _ => None,
        });

        // Decode the image
        let mut img =
            match decode_image_stream(&stream, width, height, &color_space, bits_per_component) {
                Ok(img) => img,
                Err(e) => {
                    if options.verbose {
                        log(&format!("  Skipping: Could not decode: {}", e));
                    }
                    skipped_images += 1;
                    continue;
                }
            };

        // Handle SMask
        if let Some(smask_obj_id) = smask_id {
            if let Ok(Object::Stream(smask_stream)) = doc.get_object(smask_obj_id) {
                match decode_smask_stream(smask_stream, width, height) {
                    Ok(alpha_data) => {
                        let rgb = img.to_rgb8();
                        let mut rgba_data = Vec::with_capacity((width * height * 4) as usize);

                        for (pixel, alpha) in rgb.pixels().zip(alpha_data.iter()) {
                            rgba_data.push(pixel[0]);
                            rgba_data.push(pixel[1]);
                            rgba_data.push(pixel[2]);
                            rgba_data.push(*alpha);
                        }

                        if let Some(rgba_img) = image::RgbaImage::from_raw(width, height, rgba_data)
                        {
                            img = DynamicImage::ImageRgba8(rgba_img);
                            if options.verbose {
                                log("    Decoded SMask alpha channel");
                            }
                        }
                    }
                    Err(e) => {
                        if options.verbose {
                            log(&format!("    Warning: Could not decode SMask: {}", e));
                        }
                    }
                }
            }
        }

        // Resample if needed
        let resampled = if needs_resampling {
            if options.verbose {
                log(&format!(
                    "  Resampling from {}x{} to {}x{}",
                    width, height, target_width, target_height
                ));
            }
            resample_image(&img, target_width, target_height)
        } else {
            if options.verbose {
                log("  Re-encoding as JPEG (no resize needed)");
            }
            img
        };

        // Encode
        let img_has_alpha = has_alpha(&resampled);

        if img_has_alpha {
            let (mut new_stream, smask_stream, _, _) = encode_with_alpha_stream(&resampled, options.quality)?;

            if let Some(smask) = smask_stream {
                let smask_id = doc.add_object(Object::Stream(smask));
                new_stream.dict.set("SMask", Object::Reference(smask_id));

                if options.verbose {
                    log(&format!("      Preserved alpha channel with SMask {:?}", smask_id));
                }
            }

            doc.objects.insert(object_id, Object::Stream(new_stream));
        } else {
            if options.verbose && smask_id.is_some() {
                log("      Converting opaque image to JPEG");
            }
            let (new_stream, _, _) = encode_as_jpeg_stream(&resampled, options.quality)?;
            doc.objects.insert(object_id, Object::Stream(new_stream));
        }

        resampled_images += 1;
    }

    Ok(ResampleResult {
        total_images,
        resampled_images,
        skipped_images,
    })
}

/// Resample PDF from bytes and return resampled PDF bytes
pub fn resample_pdf_bytes(
    input_bytes: &[u8],
    options: &ResampleOptions,
) -> Result<(Vec<u8>, ResampleResult), ResampleError> {
    if options.quality == 0 || options.quality > 100 {
        return Err(ResampleError::InvalidQuality);
    }

    // Step 1: Scan all content streams to find image display dimensions
    let display_info_map = {
        let doc = Document::load_mem(input_bytes)
            .map_err(|e| ResampleError::LoadError(e.to_string()))?;
        let mut scanner = ContentScanner::new(&doc, options.verbose);
        scanner.scan_all_pages();
        scanner.get_display_info_map()
    }; // doc is dropped here

    // Step 2: Reload and process images
    let mut doc = Document::load_mem(input_bytes)
        .map_err(|e| ResampleError::LoadError(e.to_string()))?;

    let log_fn = |_msg: &str| {
        #[cfg(not(target_arch = "wasm32"))]
        if options.verbose {
            println!("{}", _msg);
        }
    };

    let result = process_images_in_doc(&mut doc, &display_info_map, options, log_fn)
        .map_err(|e| ResampleError::ProcessingError(e))?;

    // Compress streams if requested
    if options.compress_streams {
        doc.compress();
    }

    // Save to bytes
    let mut output_bytes = Vec::new();
    doc.save_to(&mut output_bytes)
        .map_err(|e| ResampleError::SaveError(e.to_string()))?;

    Ok((output_bytes, result))
}

/// Extract detailed image information from a PDF, organized by page
pub fn extract_pdf_images_info(pdf_bytes: &[u8]) -> Result<Vec<PageImages>, ResampleError> {
    let doc = Document::load_mem(pdf_bytes)
        .map_err(|e| ResampleError::LoadError(e.to_string()))?;

    // Get display info for DPI calculation
    let mut scanner = ContentScanner::new(&doc, false);
    scanner.scan_all_pages();
    let display_info_map = scanner.get_display_info_map();

    // Build a map of which images appear on which pages
    let mut page_image_map: HashMap<u32, Vec<ObjectId>> = HashMap::new();
    
    let pages = doc.get_pages();
    for (page_num, &page_id) in pages.iter() {
        let page_images = collect_page_images(&doc, page_id);
        page_image_map.insert(*page_num, page_images);
    }

    // Collect all image info
    let mut result: Vec<PageImages> = Vec::new();

    for (page_num, image_ids) in page_image_map.iter() {
        let mut images: Vec<ImageInfo> = Vec::new();

        for &obj_id in image_ids {
            if let Ok(Object::Stream(stream)) = doc.get_object(obj_id) {
                let info = extract_image_info_from_stream(
                    obj_id,
                    stream,
                    &doc,
                    display_info_map.get(&obj_id),
                    false,
                );
                images.push(info);

                // Check for SMask
                if let Ok(Object::Reference(smask_id)) = stream.dict.get(b"SMask") {
                    if let Ok(Object::Stream(smask_stream)) = doc.get_object(*smask_id) {
                        let smask_info = extract_image_info_from_stream(
                            *smask_id,
                            smask_stream,
                            &doc,
                            None,
                            true,
                        );
                        images.push(smask_info);
                    }
                }
            }
        }

        if !images.is_empty() {
            result.push(PageImages {
                page_number: *page_num,
                images,
            });
        }
    }

    // Sort by page number
    result.sort_by_key(|p| p.page_number);

    Ok(result)
}

/// Extracted image data with format information
#[derive(Debug, Clone)]
pub struct ExtractedImage {
    /// Image data bytes
    pub data: Vec<u8>,
    /// Format: "jpeg" or "png"
    pub format: String,
    /// MIME type
    pub mime_type: String,
}

/// Extract a single image from a PDF in its native format when possible
/// Returns JPEG for DCTDecode images, PNG for others
/// object_id format: "num gen" e.g. "12 0"
pub fn extract_image_native(pdf_bytes: &[u8], object_id_str: &str) -> Result<ExtractedImage, ResampleError> {
    let doc = Document::load_mem(pdf_bytes)
        .map_err(|e| ResampleError::LoadError(e.to_string()))?;

    // Parse object ID
    let parts: Vec<&str> = object_id_str.split_whitespace().collect();
    if parts.len() != 2 {
        return Err(ResampleError::ProcessingError("Invalid object ID format".to_string()));
    }
    
    let obj_num: u32 = parts[0].parse()
        .map_err(|_| ResampleError::ProcessingError("Invalid object number".to_string()))?;
    let gen_num: u16 = parts[1].parse()
        .map_err(|_| ResampleError::ProcessingError("Invalid generation number".to_string()))?;
    
    let obj_id: ObjectId = (obj_num, gen_num);

    // Get the stream
    let stream = match doc.get_object(obj_id) {
        Ok(Object::Stream(s)) => s,
        _ => return Err(ResampleError::ProcessingError("Object is not an image stream".to_string())),
    };

    // Check filter type
    let filter = stream
        .dict
        .get(b"Filter")
        .ok()
        .and_then(|f| match f {
            Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
            Object::Array(arr) => arr.first().and_then(|f| match f {
                Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
                _ => None,
            }),
            _ => None,
        });

    // Check for SMask (alpha channel)
    let has_smask = stream.dict.get(b"SMask").is_ok();

    // If it's a JPEG without SMask, return the raw JPEG data
    if filter.as_deref() == Some("DCTDecode") && !has_smask {
        return Ok(ExtractedImage {
            data: stream.content.clone(),
            format: "jpeg".to_string(),
            mime_type: "image/jpeg".to_string(),
        });
    }

    // Otherwise, decode and convert to PNG
    let width = stream
        .dict
        .get(b"Width")
        .ok()
        .and_then(|w| match w {
            Object::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(0);

    let height = stream
        .dict
        .get(b"Height")
        .ok()
        .and_then(|h| match h {
            Object::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(0);

    if width == 0 || height == 0 {
        return Err(ResampleError::ProcessingError("Invalid image dimensions".to_string()));
    }

    let color_space = stream
        .dict
        .get(b"ColorSpace")
        .ok()
        .map(|cs| get_color_space_name(cs, &doc))
        .unwrap_or_else(|| "DeviceRGB".to_string());

    let bits_per_component = stream
        .dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|b| match b {
            Object::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(8);

    // Decode the image
    let img = decode_image_stream(stream, width, height, &color_space, bits_per_component)
        .map_err(|e| ResampleError::ProcessingError(e))?;

    // Check for SMask and apply alpha
    let final_img = if let Ok(Object::Reference(smask_id)) = stream.dict.get(b"SMask") {
        if let Ok(Object::Stream(smask_stream)) = doc.get_object(*smask_id) {
            match decode_smask_stream(smask_stream, width, height) {
                Ok(alpha_data) => {
                    let rgb = img.to_rgb8();
                    let mut rgba_data = Vec::with_capacity((width * height * 4) as usize);
                    for (pixel, alpha) in rgb.pixels().zip(alpha_data.iter()) {
                        rgba_data.push(pixel[0]);
                        rgba_data.push(pixel[1]);
                        rgba_data.push(pixel[2]);
                        rgba_data.push(*alpha);
                    }
                    if let Some(rgba_img) = image::RgbaImage::from_raw(width, height, rgba_data) {
                        DynamicImage::ImageRgba8(rgba_img)
                    } else {
                        img
                    }
                }
                Err(_) => img,
            }
        } else {
            img
        }
    } else {
        img
    };

    // Encode as PNG
    let mut png_bytes = Vec::new();
    final_img.write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .map_err(|e| ResampleError::ProcessingError(format!("Failed to encode PNG: {}", e)))?;

    Ok(ExtractedImage {
        data: png_bytes,
        format: "png".to_string(),
        mime_type: "image/png".to_string(),
    })
}

/// Collect all image object IDs referenced from a page
fn collect_page_images(doc: &Document, page_id: ObjectId) -> Vec<ObjectId> {
    let mut images: Vec<ObjectId> = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();

    let page_dict = match doc.get_object(page_id) {
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => return images,
    };

    // Get resources
    let resources = get_page_resources_static(doc, &page_dict, page_id);

    // Get XObjects from resources
    let xobjects = get_xobjects_static(doc, &resources);

    // Check each XObject
    for (_, &obj_id) in xobjects.iter() {
        collect_images_recursive(doc, obj_id, &mut images, &mut seen);
    }

    images
}

/// Recursively collect images from an object (handles Form XObjects)
fn collect_images_recursive(
    doc: &Document,
    obj_id: ObjectId,
    images: &mut Vec<ObjectId>,
    seen: &mut HashSet<ObjectId>,
) {
    if seen.contains(&obj_id) {
        return;
    }
    seen.insert(obj_id);

    let stream = match doc.get_object(obj_id) {
        Ok(Object::Stream(s)) => s,
        _ => return,
    };

    let subtype = stream.dict.get(b"Subtype").ok().and_then(|s| match s {
        Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
        _ => None,
    });

    match subtype.as_deref() {
        Some("Image") => {
            images.push(obj_id);
        }
        Some("Form") => {
            // Get resources from Form XObject and recurse
            if let Ok(res) = stream.dict.get(b"Resources") {
                let xobjects = get_xobjects_static(doc, res);
                for (_, &child_id) in xobjects.iter() {
                    collect_images_recursive(doc, child_id, images, seen);
                }
            }
        }
        _ => {}
    }
}

/// Get page resources (static version)
fn get_page_resources_static(doc: &Document, page_dict: &Dictionary, page_id: ObjectId) -> Object {
    if let Ok(resources) = page_dict.get(b"Resources") {
        return resources.clone();
    }

    if let Ok(Object::Reference(parent_id)) = page_dict.get(b"Parent") {
        if let Ok(Object::Dictionary(parent_dict)) = doc.get_object(*parent_id) {
            if let Ok(resources) = parent_dict.get(b"Resources") {
                return resources.clone();
            }
        }
    }

    let _ = page_id;
    Object::Null
}

/// Get XObjects from resources (static version)
fn get_xobjects_static(doc: &Document, resources: &Object) -> HashMap<String, ObjectId> {
    let mut result = HashMap::new();

    let res_dict = match resources {
        Object::Dictionary(d) => Some(d),
        Object::Reference(id) => {
            if let Ok(Object::Dictionary(d)) = doc.get_object(*id) {
                Some(d)
            } else {
                None
            }
        }
        _ => None,
    };

    if let Some(res_dict) = res_dict {
        if let Ok(xobjects) = res_dict.get(b"XObject") {
            let xobj_dict = match xobjects {
                Object::Dictionary(d) => Some(d),
                Object::Reference(id) => {
                    if let Ok(Object::Dictionary(d)) = doc.get_object(*id) {
                        Some(d)
                    } else {
                        None
                    }
                }
                _ => None,
            };

            if let Some(xobj_dict) = xobj_dict {
                for (name, value) in xobj_dict.iter() {
                    let name_str = String::from_utf8_lossy(name).to_string();
                    if let Object::Reference(obj_id) = value {
                        result.insert(name_str, *obj_id);
                    }
                }
            }
        }
    }

    result
}

/// Extract image info from a stream object
fn extract_image_info_from_stream(
    obj_id: ObjectId,
    stream: &Stream,
    doc: &Document,
    display_info: Option<&ImageDisplayInfo>,
    is_smask: bool,
) -> ImageInfo {
    let width = stream
        .dict
        .get(b"Width")
        .ok()
        .and_then(|w| match w {
            Object::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(0);

    let height = stream
        .dict
        .get(b"Height")
        .ok()
        .and_then(|h| match h {
            Object::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(0);

    let color_space = if is_smask {
        "DeviceGray".to_string()
    } else {
        stream
            .dict
            .get(b"ColorSpace")
            .ok()
            .map(|cs| get_color_space_name(cs, doc))
            .unwrap_or_else(|| "Unknown".to_string())
    };

    let bits_per_component = stream
        .dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|b| match b {
            Object::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(8);

    let filter = stream
        .dict
        .get(b"Filter")
        .ok()
        .and_then(|f| match f {
            Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
            Object::Array(arr) => arr.first().and_then(|f| match f {
                Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
                _ => None,
            }),
            _ => None,
        })
        .unwrap_or_else(|| "raw".to_string());

    let dpi_x = display_info.map(|info| info.effective_dpi_x());
    let dpi_y = display_info.map(|info| info.effective_dpi_y());

    ImageInfo {
        object_id: (obj_id.0, obj_id.1),
        image_type: if is_smask { "smask".to_string() } else { "image".to_string() },
        width,
        height,
        color_space,
        bits_per_component,
        filter,
        size_bytes: stream.content.len(),
        dpi_x,
        dpi_y,
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub mod file_ops {
    use super::*;
    use std::path::Path;

    /// Resample PDF from file path to file path
    pub fn resample_pdf_file(
        input_path: &Path,
        output_path: &Path,
        options: &ResampleOptions,
    ) -> Result<ResampleResult, ResampleError> {
        if options.quality == 0 || options.quality > 100 {
            return Err(ResampleError::InvalidQuality);
        }

        // Step 1: Scan all content streams to find image display dimensions
        let display_info_map = {
            let doc = Document::load(input_path)
                .map_err(|e| ResampleError::LoadError(format!("{:?}: {}", input_path, e)))?;
            let mut scanner = ContentScanner::new(&doc, options.verbose);
            scanner.scan_all_pages();
            let map = scanner.get_display_info_map();

            if options.verbose {
                println!("\nFound display info for {} images", map.len());
                for (id, info) in &map {
                    println!(
                        "  {:?}: {}x{} px @ {:.1}x{:.1} pt = {:.1} DPI",
                        id,
                        info.pixel_width,
                        info.pixel_height,
                        info.display_width_points,
                        info.display_height_points,
                        info.max_effective_dpi()
                    );
                }
            }
            map
        }; // doc is dropped here

        // Step 2: Process images
        let mut doc = Document::load(input_path)
            .map_err(|e| ResampleError::LoadError(format!("{:?}: {}", input_path, e)))?;

        let log_fn = |msg: &str| {
            if options.verbose {
                println!("{}", msg);
            }
        };

        let result = process_images_in_doc(&mut doc, &display_info_map, options, log_fn)
            .map_err(|e| ResampleError::ProcessingError(e))?;

        // Compress streams if requested
        if options.compress_streams {
            doc.compress();
        }

        // Save
        doc.save(output_path)
            .map_err(|e| ResampleError::SaveError(format!("{:?}: {}", output_path, e)))?;

        Ok(result)
    }
}

