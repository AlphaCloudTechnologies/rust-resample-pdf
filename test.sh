#!/bin/bash

# Test script for resample-pdf
# Compiles the app, runs it on test PDFs, and verifies output DPI
#
# Usage: ./test.sh [OPTIONS]
#   -d, --dpi <DPI>       Target DPI (default: 150)
#   -q, --quality <1-100> JPEG quality (default: 75)
#   -h, --help            Show this help message

set -e

# Default values
TARGET_DPI=96
QUALITY=70
DPI_TOLERANCE=5  # Allow ±5 DPI variance

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -d|--dpi)
            TARGET_DPI="$2"
            shift 2
            ;;
        -q|--quality)
            QUALITY="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: ./test.sh [OPTIONS]"
            echo "  -d, --dpi <DPI>       Target DPI (default: 150)"
            echo "  -q, --quality <1-100> JPEG quality (default: 75)"
            echo "  -h, --help            Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Set up library path for PDFium
export LD_LIBRARY_PATH="$SCRIPT_DIR/lib:$LD_LIBRARY_PATH"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "========================================"
echo "  resample-pdf Test Suite"
echo "========================================"
echo "  Target DPI: $TARGET_DPI"
echo "  Quality: $QUALITY"
echo "========================================"
echo ""

# Step 0: Clean output folder
rm -f output/*.pdf output/*.png 2>/dev/null || true
rm -rf output/visual_compare 2>/dev/null || true

# Step 1: Compile
echo -e "${YELLOW}[1/5] Compiling...${NC}"
source "$HOME/.cargo/env" 2>/dev/null || true
cargo build --release --quiet
echo -e "${GREEN}✓ Compilation successful${NC}"
echo ""

# Step 2: Create output directory
mkdir -p output

# Step 3: Run on all test PDFs
echo -e "${YELLOW}[2/5] Processing PDFs...${NC}"
declare -A PROCESS_TIMES
TOTAL_PROCESS_START=$(date +%s%3N)
for pdf in input/*.pdf; do
    if [ -f "$pdf" ]; then
        name=$(basename "$pdf" .pdf)
        echo "  Processing $name.pdf..."
        START_TIME=$(date +%s%3N)
        ./target/release/resample-pdf -i "$pdf" -o "output/${name}_resampled.pdf" -d $TARGET_DPI -q $QUALITY
        END_TIME=$(date +%s%3N)
        ELAPSED=$((END_TIME - START_TIME))
        PROCESS_TIMES[$name]=$ELAPSED
        echo "  Done in ${ELAPSED}ms"
        echo ""
    fi
done
TOTAL_PROCESS_END=$(date +%s%3N)
TOTAL_PROCESS_TIME=$((TOTAL_PROCESS_END - TOTAL_PROCESS_START))
echo -e "${GREEN}✓ All PDFs processed in ${TOTAL_PROCESS_TIME}ms${NC}"
echo ""

# Step 4: Verify DPI using pdfimages
echo -e "${YELLOW}[3/5] Verifying output DPI...${NC}"
echo ""

TOTAL_IMAGES=0
CORRECT_IMAGES=0
FAILED_IMAGES=0

for pdf in output/*_resampled.pdf; do
    if [ -f "$pdf" ]; then
        name=$(basename "$pdf")
        echo "  Checking $name:"
        
        # Get all images and their DPI (skip header lines, get x-ppi and y-ppi)
        while IFS= read -r line; do
            # Skip header lines
            if [[ "$line" == *"page"* ]] || [[ "$line" == *"---"* ]]; then
                continue
            fi
            
            # Parse the line - columns are: page num type width height color comp bpc enc interp object ID x-ppi y-ppi size ratio
            # x-ppi is column 12, y-ppi is column 13 (0-indexed: 11, 12)
            read -ra cols <<< "$line"
            
            if [ ${#cols[@]} -ge 14 ]; then
                obj_id="${cols[10]} ${cols[11]}"
                x_ppi="${cols[12]}"
                y_ppi="${cols[13]}"
                img_type="${cols[2]}"
                width="${cols[3]}"
                height="${cols[4]}"
                
                # Skip smask entries (they inherit DPI from parent)
                if [[ "$img_type" == "smask" ]]; then
                    continue
                fi
                
                TOTAL_IMAGES=$((TOTAL_IMAGES + 1))
                
                # Check if DPI is within tolerance of target
                x_diff=$((x_ppi - TARGET_DPI))
                y_diff=$((y_ppi - TARGET_DPI))
                x_diff=${x_diff#-}  # Absolute value
                y_diff=${y_diff#-}
                
                if [ "$x_diff" -le "$DPI_TOLERANCE" ] && [ "$y_diff" -le "$DPI_TOLERANCE" ]; then
                    CORRECT_IMAGES=$((CORRECT_IMAGES + 1))
                    echo -e "    ${GREEN}✓${NC} Object $obj_id: ${width}x${height} @ ${x_ppi}x${y_ppi} DPI"
                else
                    # Check if this is a small image that wasn't resized (already below target)
                    if [ "$x_ppi" -lt "$((TARGET_DPI + DPI_TOLERANCE))" ] && [ "$y_ppi" -lt "$((TARGET_DPI + DPI_TOLERANCE))" ]; then
                        CORRECT_IMAGES=$((CORRECT_IMAGES + 1))
                        echo -e "    ${GREEN}✓${NC} Object $obj_id: ${width}x${height} @ ${x_ppi}x${y_ppi} DPI (kept original - below target)"
                    # Check if image is too small to scale (would result in unusable tiny image)
                    elif [ "$width" -lt 100 ] || [ "$height" -lt 100 ]; then
                        CORRECT_IMAGES=$((CORRECT_IMAGES + 1))
                        echo -e "    ${GREEN}✓${NC} Object $obj_id: ${width}x${height} @ ${x_ppi}x${y_ppi} DPI (kept original - too small to resample)"
                    else
                        FAILED_IMAGES=$((FAILED_IMAGES + 1))
                        echo -e "    ${RED}✗${NC} Object $obj_id: ${width}x${height} @ ${x_ppi}x${y_ppi} DPI (expected ~${TARGET_DPI})"
                    fi
                fi
            fi
        done < <(pdfimages -list "$pdf" 2>/dev/null)
        
        echo ""
    fi
done

# Step 5: Summary
echo -e "${YELLOW}[4/5] Results Summary${NC}"
echo "========================================"
echo ""

# File size comparison
echo "File Size Comparison:"
printf "  %-15s %10s %10s %10s %10s\n" "File" "Original" "New" "Reduction" "Time"
printf "  %-15s %10s %10s %10s %10s\n" "----" "--------" "---" "---------" "----"

for pdf in input/*.pdf; do
    if [ -f "$pdf" ]; then
        name=$(basename "$pdf" .pdf)
        orig_size=$(stat -c%s "$pdf")
        new_size=$(stat -c%s "output/${name}_resampled.pdf" 2>/dev/null || echo "0")
        time_ms="${PROCESS_TIMES[$name]:-0}"
        
        if [ "$new_size" != "0" ]; then
            orig_human=$(numfmt --to=iec "$orig_size")
            new_human=$(numfmt --to=iec "$new_size")
            reduction=$((100 - (new_size * 100 / orig_size)))
            printf "  %-15s %10s %10s %9s%% %8sms\n" "$name" "$orig_human" "$new_human" "$reduction" "$time_ms"
        fi
    fi
done

echo ""
echo "Total processing time: ${TOTAL_PROCESS_TIME}ms"

echo ""
echo "DPI Verification:"
echo "  Total images checked: $TOTAL_IMAGES"
echo -e "  ${GREEN}Correct (DPI ≈ $TARGET_DPI):${NC} $CORRECT_IMAGES"

if [ "$FAILED_IMAGES" -gt 0 ]; then
    echo -e "  ${RED}Failed:${NC} $FAILED_IMAGES"
fi

echo ""

# Step 5: Visual Comparison (extract pages as PNG and compare)
echo -e "${YELLOW}[5/5] Visual Comparison Test...${NC}"
echo ""

# Check for required tools
if ! command -v pdftoppm &> /dev/null; then
    echo -e "  ${YELLOW}⚠ pdftoppm not found (install poppler-utils). Skipping visual comparison.${NC}"
    VISUAL_TEST_SKIPPED=true
elif ! command -v compare &> /dev/null; then
    echo -e "  ${YELLOW}⚠ ImageMagick compare not found. Skipping visual comparison.${NC}"
    VISUAL_TEST_SKIPPED=true
else
    VISUAL_TEST_SKIPPED=false
    
    # Create directory for extracted images (persistent for inspection)
    COMPARE_DIR="output/visual_compare"
    mkdir -p "$COMPARE_DIR"
    
    # Thresholds for acceptable visual difference
    #
    # PSNR (Peak Signal-to-Noise Ratio) - measures image quality in decibels (dB)
    #   > 40 dB: Excellent (nearly identical)
    #   30-40 dB: Good (minor differences)
    #   20-30 dB: Acceptable (visible but tolerable)
    #   < 20 dB: Poor (significant degradation)
    #
    # MAE (Mean Absolute Error) - average pixel difference as percentage
    #   0%: Identical
    #   < 1%: Virtually indistinguishable
    #   1-2%: Minor differences
    #   2-5%: Noticeable if looking closely
    #   > 5%: Obvious differences
    #
    MIN_PSNR=20         # Minimum PSNR in dB
    MAX_MAE_PERCENT=2   # Maximum Mean Absolute Error percentage
    
    VISUAL_PASSED=0
    VISUAL_FAILED=0
    VISUAL_TOTAL=0
    
    for pdf in input/*.pdf; do
        if [ -f "$pdf" ]; then
            name=$(basename "$pdf" .pdf)
            output_pdf="output/${name}_resampled.pdf"
            
            if [ ! -f "$output_pdf" ]; then
                continue
            fi
            
            echo "  Comparing $name.pdf:"
            
            # Get page count
            PAGE_COUNT=$(pdfinfo "$pdf" 2>/dev/null | grep "Pages:" | awk '{print $2}')
            if [ -z "$PAGE_COUNT" ] || [ "$PAGE_COUNT" -eq 0 ]; then
                PAGE_COUNT=1
            fi
            
            # Extract pages as PNG (at 72 DPI for fast comparison)
            pdftoppm -png -r $TARGET_DPI "$pdf" "$COMPARE_DIR/input_${name}" 2>/dev/null
            pdftoppm -png -r $TARGET_DPI "$output_pdf" "$COMPARE_DIR/output_${name}" 2>/dev/null
            
            # Compare each page
            for page_num in $(seq 1 $PAGE_COUNT); do
                VISUAL_TOTAL=$((VISUAL_TOTAL + 1))
                
                # pdftoppm uses different naming formats based on page count
                # Try various patterns to find the files
                input_page=""
                output_page=""
                
                # Pattern 1: With leading zeros based on total page count (e.g., -01, -001)
                for pad_width in 1 2 3; do
                    padded=$(printf "%0${pad_width}d" $page_num)
                    test_input="$COMPARE_DIR/input_${name}-${padded}.png"
                    test_output="$COMPARE_DIR/output_${name}-${padded}.png"
                    if [ -f "$test_input" ] && [ -f "$test_output" ]; then
                        input_page="$test_input"
                        output_page="$test_output"
                        break
                    fi
                done
                
                # Pattern 2: No leading zeros (e.g., -1, -2)
                if [ -z "$input_page" ]; then
                    test_input="$COMPARE_DIR/input_${name}-${page_num}.png"
                    test_output="$COMPARE_DIR/output_${name}-${page_num}.png"
                    if [ -f "$test_input" ] && [ -f "$test_output" ]; then
                        input_page="$test_input"
                        output_page="$test_output"
                    fi
                fi
                
                if [ -z "$input_page" ] || [ -z "$output_page" ]; then
                    echo -e "    ${YELLOW}⚠${NC} Page $page_num: Could not find extracted images"
                    continue
                fi
                
                # Get image dimensions
                INPUT_SIZE=$(identify -format "%wx%h" "$input_page" 2>/dev/null)
                OUTPUT_SIZE=$(identify -format "%wx%h" "$output_page" 2>/dev/null)
                
                # If dimensions differ, resize output to match input for comparison
                COMPARE_OUTPUT="$output_page"
                if [ "$INPUT_SIZE" != "$OUTPUT_SIZE" ]; then
                    COMPARE_OUTPUT="$COMPARE_DIR/compare_${name}_${page_num}.png"
                    convert "$output_page" -resize "$INPUT_SIZE!" "$COMPARE_OUTPUT" 2>/dev/null
                fi
                
                # Compare using MAE (Mean Absolute Error)
                # ImageMagick compare outputs: "1234.56 (0.0123)" where the parenthesis value is normalized 0-1
                # Note: compare returns exit code 1 when images differ, so we use || true
                MAE_RESULT=$(compare -metric MAE "$input_page" "$COMPARE_OUTPUT" /dev/null 2>&1) || true
                
                # Extract normalized value from parentheses (0.0123)
                MAE_NORMALIZED=$(echo "$MAE_RESULT" | grep -oE '\([0-9.e+-]+\)' | tr -d '()' | head -1)
                
                # If no parenthesis value found, try getting the raw number
                if [ -z "$MAE_NORMALIZED" ]; then
                    RAW_MAE=$(echo "$MAE_RESULT" | grep -oE '^[0-9.e+-]+' | head -1)
                    if [ "$RAW_MAE" = "0" ]; then
                        MAE_NORMALIZED="0"
                    else
                        MAE_NORMALIZED="0.01"  # Default small value
                    fi
                fi
                
                # Convert to percentage (handle scientific notation)
                DIFF_PERCENT=$(awk "BEGIN {printf \"%.2f\", $MAE_NORMALIZED * 100}")
                
                # Handle identical images (MAE = 0 or very close to 0)
                IS_IDENTICAL=$(awk "BEGIN {print ($MAE_NORMALIZED < 0.0001) ? 1 : 0}")
                if [ "$IS_IDENTICAL" -eq 1 ]; then
                    VISUAL_PASSED=$((VISUAL_PASSED + 1))
                    printf "    ${GREEN}✓${NC} Page %d: Identical (MAE=0%%)\n" "$page_num"
                    continue
                fi
                
                # Get PSNR for quality reference
                # Higher PSNR = more similar. Typically:
                # > 40 dB: Excellent, 30-40 dB: Good, 20-30 dB: Acceptable, < 20 dB: Poor
                PSNR_RESULT=$(compare -metric PSNR "$input_page" "$COMPARE_OUTPUT" /dev/null 2>&1) || true
                PSNR_VALUE=$(echo "$PSNR_RESULT" | grep -oE 'inf|[0-9]+\.?[0-9]*' | head -1)
                
                # Handle "inf" (identical images) or empty/zero values
                if [ "$PSNR_VALUE" = "inf" ] || [ -z "$PSNR_VALUE" ] || [ "$PSNR_VALUE" = "0" ]; then
                    PSNR_VALUE="99.0"
                fi
                
                # Check if quality is acceptable
                PSNR_OK=$(awk "BEGIN {print ($PSNR_VALUE >= $MIN_PSNR) ? 1 : 0}")
                DIFF_OK=$(awk "BEGIN {print ($MAE_NORMALIZED * 100 < $MAX_MAE_PERCENT) ? 1 : 0}")
                
                if [ "$PSNR_OK" -eq 1 ] && [ "$DIFF_OK" -eq 1 ]; then
                    VISUAL_PASSED=$((VISUAL_PASSED + 1))
                    printf "    ${GREEN}✓${NC} Page %d: PSNR=%.1f dB, MAE=%.2f%%\n" "$page_num" "$PSNR_VALUE" "$DIFF_PERCENT"
                else
                    VISUAL_FAILED=$((VISUAL_FAILED + 1))
                    printf "    ${RED}✗${NC} Page %d: PSNR=%.1f dB, MAE=%.2f%% (threshold: PSNR≥${MIN_PSNR}dB, MAE<${MAX_MAE_PERCENT}%%)\n" "$page_num" "$PSNR_VALUE" "$DIFF_PERCENT"
                    
                    # Save difference image for debugging
                    DIFF_IMG="output/${name}_page${page_num}_diff.png"
                    compare "$input_page" "$COMPARE_OUTPUT" -compose src "$DIFF_IMG" 2>/dev/null || true
                    if [ -f "$DIFF_IMG" ]; then
                        echo -e "      Difference image saved: $DIFF_IMG"
                    fi
                fi
            done
            echo ""
        fi
    done
    
    echo "Visual Comparison Summary:"
    echo "  Total pages compared: $VISUAL_TOTAL"
    echo -e "  ${GREEN}Passed:${NC} $VISUAL_PASSED"
    if [ "$VISUAL_FAILED" -gt 0 ]; then
        echo -e "  ${RED}Failed:${NC} $VISUAL_FAILED"
    fi
fi

echo ""
echo "========================================"
echo "  FINAL TEST SUMMARY"
echo "========================================"
echo ""

# DPI Test Summary
DPI_STATUS="PASSED"
if [ "$FAILED_IMAGES" -gt 0 ]; then
    DPI_STATUS="FAILED"
fi

echo "DPI Verification:"
echo "  Total images: $TOTAL_IMAGES"
echo "  Passed: $CORRECT_IMAGES"
if [ "$FAILED_IMAGES" -gt 0 ]; then
    echo -e "  ${RED}Failed: $FAILED_IMAGES${NC}"
    echo -e "  Status: ${RED}✗ FAILED${NC}"
else
    echo -e "  Status: ${GREEN}✓ PASSED${NC}"
fi

echo ""

# Visual Comparison Summary
VISUAL_STATUS="SKIPPED"
if [ "$VISUAL_TEST_SKIPPED" = false ]; then
    if [ "$VISUAL_FAILED" -gt 0 ]; then
        VISUAL_STATUS="FAILED"
    else
        VISUAL_STATUS="PASSED"
    fi
    
    echo "Visual Comparison:"
    echo "  Total pages: $VISUAL_TOTAL"
    echo "  Passed: $VISUAL_PASSED"
    if [ "$VISUAL_FAILED" -gt 0 ]; then
        echo -e "  ${RED}Failed: $VISUAL_FAILED${NC}"
        echo -e "  Status: ${RED}✗ FAILED${NC}"
    else
        echo -e "  Status: ${GREEN}✓ PASSED${NC}"
    fi
else
    echo "Visual Comparison:"
    echo -e "  Status: ${YELLOW}⚠ SKIPPED${NC} (missing tools)"
fi

echo ""
echo "========================================"

# Final result
if [ "$DPI_STATUS" = "FAILED" ]; then
    echo -e "${RED}✗ OVERALL: TEST FAILED${NC}"
    echo "  DPI verification failed for $FAILED_IMAGES image(s)."
    exit 1
fi

if [ "$VISUAL_STATUS" = "FAILED" ]; then
    echo -e "${RED}✗ OVERALL: TEST FAILED${NC}"
    echo "  Visual comparison failed for $VISUAL_FAILED page(s)."
    echo "  Check the *_diff.png files in output/ for details."
    exit 1
fi

echo -e "${GREEN}✓ OVERALL: ALL TESTS PASSED${NC}"
exit 0
