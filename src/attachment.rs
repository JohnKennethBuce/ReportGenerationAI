//! File attachment support for DayRecap Chatbox.
//! Supports Word (.docx), Excel (.xlsx), PDF (.pdf), Images (.png, .jpg, etc.), and text/code files.

use crate::office;
use flate2::read::ZlibDecoder;
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
pub struct Attachment {
    #[allow(dead_code)]
    pub path: PathBuf,
    pub filename: String,
    #[allow(dead_code)]
    pub extension: String,
    pub summary: String,
    pub extracted_content: String,
}

impl Attachment {
    pub fn from_path<P: AsRef<Path>>(p: P) -> Result<Self, String> {
        let path = p.as_ref().to_path_buf();
        if !path.exists() {
            return Err(format!("File does not exist: {}", path.display()));
        }

        let filename = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attached_file".into());

        let extension = path
            .extension()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        let (summary, extracted_content) = match extension.as_str() {
            "docx" => match office::dissect_file(&path) {
                Ok(doc) => {
                    let s = format!(
                        "Word Document `{}`: Columns [{}] ({} sample rows)",
                        filename,
                        doc.detected_columns.join(" | "),
                        doc.sample_rows.len()
                    );
                    let mut ec = format!(
                        "Table Columns: {}\nSample Rows:\n",
                        doc.detected_columns.join(" | ")
                    );
                    for (i, row) in doc.sample_rows.iter().enumerate() {
                        ec += &format!("  Row {}: {}\n", i + 1, row.join(" | "));
                    }
                    (s, ec)
                }
                Err(e) => (
                    format!("Word Document `{}`", filename),
                    format!("Could not dissect table: {e}"),
                ),
            },
            "xlsx" => match office::dissect_file(&path) {
                Ok(doc) => {
                    let s = format!(
                        "Excel Spreadsheet `{}`: Columns [{}] ({} sample rows)",
                        filename,
                        doc.detected_columns.join(" | "),
                        doc.sample_rows.len()
                    );
                    let mut ec = format!(
                        "Table Columns: {}\nSample Rows:\n",
                        doc.detected_columns.join(" | ")
                    );
                    for (i, row) in doc.sample_rows.iter().enumerate() {
                        ec += &format!("  Row {}: {}\n", i + 1, row.join(" | "));
                    }
                    (s, ec)
                }
                Err(e) => (
                    format!("Excel Spreadsheet `{}`", filename),
                    format!("Could not dissect table: {e}"),
                ),
            },
            "pdf" => match extract_pdf_text(&path) {
                Ok(txt) => {
                    let char_count = txt.chars().count();
                    (
                        format!("PDF Document `{}` ({} characters extracted)", filename, char_count),
                        txt,
                    )
                }
                Err(e) => (
                    format!("PDF Document `{}`", filename),
                    format!("Could not extract text: {e}"),
                ),
            },
            "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp" => {
                let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                (
                    format!("Image File `{}` ({:.1} KB)", filename, size as f64 / 1024.0),
                    format!(
                        "Attached image file: {}\nFile size: {} bytes\n(Use image path directly for analysis or inspection).",
                        path.display(),
                        size
                    ),
                )
            }
            _ => match fs::read(&path) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let lines: Vec<&str> = text.lines().take(400).collect();
                    (
                        format!(
                            "Text/Code File `{}` ({} lines, {:.1} KB)",
                            filename,
                            lines.len(),
                            bytes.len() as f64 / 1024.0
                        ),
                        lines.join("\n"),
                    )
                }
                Err(e) => (
                    format!("File `{}`", filename),
                    format!("Could not read file: {e}"),
                ),
            },
        };

        Ok(Attachment {
            path,
            filename,
            extension,
            summary,
            extracted_content,
        })
    }

    pub fn to_prompt_context(&self) -> String {
        format!(
            "\n--- ATTACHED FILE: {} ({}) ---\n{}\n--- END ATTACHED FILE ---\n",
            self.filename, self.summary, self.extracted_content
        )
    }
}

/// Lightweight, pure-Rust PDF text extractor.
/// Scans PDF objects for decompressed streams and extracts text operators (`Tj`, `TJ`, and parenthesized text).
pub fn extract_pdf_text(path: &Path) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| format!("Cannot open PDF: {e}"))?;
    let mut data = Vec::new();
    f.read_to_end(&mut data)
        .map_err(|e| format!("Cannot read PDF: {e}"))?;

    let mut extracted = String::new();

    // 1. Scan for `/Filter /FlateDecode` streams and decompress them
    let mut idx = 0;
    while let Some(stream_start_rel) = find_subsequence(&data[idx..], b"stream") {
        let stream_start = idx + stream_start_rel + 6;
        // skip \r or \n
        let content_start = if stream_start < data.len() && data[stream_start] == b'\r' {
            if stream_start + 1 < data.len() && data[stream_start + 1] == b'\n' {
                stream_start + 2
            } else {
                stream_start + 1
            }
        } else if stream_start < data.len() && data[stream_start] == b'\n' {
            stream_start + 1
        } else {
            stream_start
        };

        if let Some(stream_end_rel) = find_subsequence(&data[content_start..], b"endstream") {
            let content_end = content_start + stream_end_rel;
            let stream_slice = &data[content_start..content_end];

            // Attempt zlib decompress
            let mut zlib = ZlibDecoder::new(stream_slice);
            let mut decompressed = Vec::new();
            if zlib.read_to_end(&mut decompressed).is_ok() {
                parse_pdf_stream_text(&decompressed, &mut extracted);
            } else {
                // If uncompressed plain text stream
                parse_pdf_stream_text(stream_slice, &mut extracted);
            }

            idx = content_end + 9;
        } else {
            break;
        }
    }

    // 2. If no streams produced text, scan top-level for parenthesized strings
    if extracted.trim().is_empty() {
        parse_pdf_stream_text(&data, &mut extracted);
    }

    if extracted.trim().is_empty() {
        Ok(format!(
            "PDF file {} (binary structure with no raw ASCII text streams found)",
            path.display()
        ))
    } else {
        Ok(extracted.trim().to_string())
    }
}

fn parse_pdf_stream_text(stream: &[u8], out: &mut String) {
    let mut i = 0;
    while i < stream.len() {
        if stream[i] == b'(' {
            i += 1;
            let mut paren_depth = 1;
            let mut s = Vec::new();
            while i < stream.len() && paren_depth > 0 {
                let b = stream[i];
                if b == b'\\' && i + 1 < stream.len() {
                    i += 1;
                    s.push(stream[i]);
                } else if b == b'(' {
                    paren_depth += 1;
                    s.push(b);
                } else if b == b')' {
                    paren_depth -= 1;
                    if paren_depth > 0 {
                        s.push(b);
                    }
                } else {
                    s.push(b);
                }
                i += 1;
            }
            let text_chunk = String::from_utf8_lossy(&s);
            let trimmed = text_chunk.trim();
            if !trimmed.is_empty() && trimmed.chars().any(char::is_alphanumeric) {
                out.push_str(trimmed);
                out.push(' ');
            }
        } else {
            i += 1;
        }
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_text_attachment() {
        let tmp = std::env::temp_dir().join("dayrecap_test_attach.txt");
        let _ = fs::write(&tmp, "Hello from DayRecap attached file test!\nLine 2 content.");
        let att = Attachment::from_path(&tmp).unwrap();
        assert_eq!(att.filename, "dayrecap_test_attach.txt");
        assert_eq!(att.extension, "txt");
        assert!(att.extracted_content.contains("Hello from DayRecap"));
        let _ = fs::remove_file(&tmp);
    }
}
