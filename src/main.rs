//! PDF Image Resampler CLI
//!
//! Command-line interface for resampling images in PDFs.

use clap::Parser;
use resample_pdf::{file_ops::resample_pdf_file, ResampleOptions};
use std::path::PathBuf;

/// Resample images in a PDF to a target DPI
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Input PDF file path
    #[arg(short, long)]
    input: PathBuf,

    /// Output PDF file path
    #[arg(short, long)]
    output: PathBuf,

    /// Target DPI for images (based on display dimensions)
    #[arg(short, long, default_value = "150")]
    dpi: f32,

    /// JPEG quality (1-100, only affects images without alpha)
    #[arg(short, long, default_value = "75")]
    quality: u8,

    /// Minimum DPI threshold - only resample images above this DPI
    #[arg(long, default_value = "0")]
    min_dpi: f32,

    /// Compress PDF streams (reduces file size)
    #[arg(short, long, default_value = "true")]
    compress_streams: bool,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let options = ResampleOptions {
        target_dpi: args.dpi,
        quality: args.quality,
        min_dpi: args.min_dpi,
        compress_streams: args.compress_streams,
        verbose: args.verbose,
    };

    println!("PDF Image Resampler");
    println!("===================");

    if args.verbose {
        println!("\nStep 1: Scanning content streams for image display dimensions...");
    }

    let result = resample_pdf_file(&args.input, &args.output, &options)?;

    println!(
        "\nDone! Processed {} images: {} resampled, {} skipped",
        result.total_images, result.resampled_images, result.skipped_images
    );
    println!("Output saved to: {:?}", args.output);

    Ok(())
}
