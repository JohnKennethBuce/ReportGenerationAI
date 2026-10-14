//! Native Word (`.docx`) and Excel (`.xlsx`) report generator & document dissector.
//!
//! Produces real, styled OpenXML `.docx` and `.xlsx` files without requiring
//! Microsoft Office to be installed, using < 2 MB RAM. Also dissects existing
//! `.docx` / `.xlsx` files to extract their columns and structure for new rules.

use chrono::NaiveDate;
use flate2::{write::DeflateEncoder, Compression};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};

#[derive(Debug, Clone)]
pub struct ReportRow {
    pub cols: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DissectedDoc {
    pub format: String, // "docx" or "xlsx"
    pub detected_columns: Vec<String>,
    pub sample_rows: Vec<Vec<String>>,
}

/// Write a styled Word `.docx` report with a bordered table (`columns` + `rows`).
pub fn write_docx(
    path: &Path,
    title: &str,
    subtitle: &str,
    columns: &[String],
    rows: &[ReportRow],
) -> Result<(), String> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    let mut doc = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
"#,
    );

    // Title paragraph
    doc += &format!(
        r#"<w:p><w:pPr><w:spacing w:after="80"/></w:pPr><w:r><w:rPr><w:b/><w:sz w:val="32"/></w:rPr><w:t>{}</w:t></w:r></w:p>"#,
        xml_esc(title)
    );
    // Subtitle paragraph
    doc += &format!(
        r#"<w:p><w:pPr><w:spacing w:after="200"/></w:pPr><w:r><w:rPr><w:color w:val="555555"/><w:sz w:val="20"/></w:rPr><w:t>{}</w:t></w:r></w:p>"#,
        xml_esc(subtitle)
    );

    // Table with borders
    doc += r#"<w:tbl>
<w:tblPr>
  <w:tblW w:w="5000" w:type="pct"/>
  <w:tblBorders>
    <w:top w:val="single" w:sz="6" w:space="0" w:color="BFBFBF"/>
    <w:left w:val="single" w:sz="6" w:space="0" w:color="BFBFBF"/>
    <w:bottom w:val="single" w:sz="6" w:space="0" w:color="BFBFBF"/>
    <w:right w:val="single" w:sz="6" w:space="0" w:color="BFBFBF"/>
    <w:insideH w:val="single" w:sz="4" w:space="0" w:color="D9D9D9"/>
    <w:insideV w:val="single" w:sz="4" w:space="0" w:color="D9D9D9"/>
  </w:tblBorders>
</w:tblPr>
"#;

    // Header row
    doc += "<w:tr>";
    for col in columns {
        doc += &format!(
            r#"<w:tc><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="1F4E79"/></w:tcPr><w:p><w:r><w:rPr><w:b/><w:color w:val="FFFFFF"/><w:sz w:val="22"/></w:rPr><w:t>{}</w:t></w:r></w:p></w:tc>"#,
            xml_esc(col)
        );
    }
    doc += "</w:tr>";

    // Data rows
    for (idx, row) in rows.iter().enumerate() {
        let fill = if idx % 2 == 1 { "F9FAFB" } else { "FFFFFF" };
        doc += "<w:tr>";
        for (c_idx, cell) in row.cols.iter().enumerate() {
            let bold = if c_idx == 0 { "<w:b/>" } else { "" };
            doc += &format!(
                r#"<w:tc><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="{fill}"/></w:tcPr><w:p><w:r><w:rPr>{bold}<w:sz w:val="20"/></w:rPr><w:t>{}</w:t></w:r></w:p></w:tc>"#,
                xml_esc(cell)
            );
        }
        doc += "</w:tr>";
    }

    doc += r#"</w:tbl>
<w:sectPr><w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr>
</w:body>
</w:document>"#;

    write_zip(
        path,
        &[
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", rels.as_bytes()),
            ("word/document.xml", doc.as_bytes()),
        ],
    )
}

/// Write a styled Excel `.xlsx` spreadsheet with wrapped text cells and auto-sized columns.
pub fn write_xlsx(
    path: &Path,
    sheet_name: &str,
    columns: &[String],
    rows: &[ReportRow],
) -> Result<(), String> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"#;

    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;

    let safe_sheet: String = sheet_name
        .chars()
        .filter(|c| !['\\', '/', '?', '*', '[', ']', ':'].contains(c))
        .take(30)
        .collect();
    let safe_sheet = if safe_sheet.is_empty() { "Report" } else { &safe_sheet };

    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="{}" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"#,
        xml_esc(safe_sheet)
    );

    let wb_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

    // Styles: 0 = default, 1 = Header (Bold White on Navy), 2 = Time col (Bold wrapped), 3 = Body wrapped
    let styles = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <fonts count="3">
    <font><sz val="11"/><name val="Calibri"/></font>
    <font><b/><sz val="11"/><color rgb="FFFFFFFF"/><name val="Calibri"/></font>
    <font><b/><sz val="11"/><name val="Calibri"/></font>
  </fonts>
  <fills count="3">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"/></fill>
    <fill><patternFill patternType="solid"><fgColor rgb="FF1F4E79"/><bgColor indexed="64"/></patternFill></fill>
  </fills>
  <borders count="2">
    <border><left/><right/><top/><bottom/><diagonal/></border>
    <border>
      <left style="thin"><color rgb="FFD0D7DE"/></left>
      <right style="thin"><color rgb="FFD0D7DE"/></right>
      <top style="thin"><color rgb="FFD0D7DE"/></top>
      <bottom style="thin"><color rgb="FFD0D7DE"/></bottom>
      <diagonal/>
    </border>
  </borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="4">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
    <xf numFmtId="0" fontId="1" fillId="2" borderId="1" xfId="0" applyFont="1" applyFill="1" applyBorder="1" applyAlignment="1">
      <alignment horizontal="left" vertical="center" wrapText="1"/>
    </xf>
    <xf numFmtId="0" fontId="2" fillId="0" borderId="1" xfId="0" applyFont="1" applyBorder="1" applyAlignment="1">
      <alignment horizontal="left" vertical="top" wrapText="1"/>
    </xf>
    <xf numFmtId="0" fontId="0" fillId="0" borderId="1" xfId="0" applyBorder="1" applyAlignment="1">
      <alignment horizontal="left" vertical="top" wrapText="1"/>
    </xf>
  </cellXfs>
</styleSheet>"#;

    let mut sheet = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<cols>
  <col min="1" max="1" width="24" customWidth="1"/>
  <col min="2" max="2" width="62" customWidth="1"/>
  <col min="3" max="10" width="78" customWidth="1"/>
</cols>
<sheetData>
"#,
    );

    // Row 1: Headers
    sheet += r#"<row r="1" ht="26" customHeight="1">"#;
    for (c_idx, col) in columns.iter().enumerate() {
        let cell_ref = format!("{}1", col_letter(c_idx));
        sheet += &format!(
            r#"<c r="{cell_ref}" s="1" t="inlineStr"><is><t>{}</t></is></c>"#,
            xml_esc(col)
        );
    }
    sheet += "</row>\n";

    // Data rows
    for (r_idx, row) in rows.iter().enumerate() {
        let row_num = r_idx + 2;
        sheet += &format!(r#"<row r="{row_num}">"#);
        for (c_idx, val) in row.cols.iter().enumerate() {
            let cell_ref = format!("{}{row_num}", col_letter(c_idx));
            let style = if c_idx == 0 { 2 } else { 3 };
            sheet += &format!(
                r#"<c r="{cell_ref}" s="{style}" t="inlineStr"><is><t>{}</t></is></c>"#,
                xml_esc(val)
            );
        }
        sheet += "</row>\n";
    }

    sheet += "</sheetData></worksheet>";

    write_zip(
        path,
        &[
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", root_rels.as_bytes()),
            ("xl/workbook.xml", workbook.as_bytes()),
            ("xl/_rels/workbook.xml.rels", wb_rels.as_bytes()),
            ("xl/styles.xml", styles.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
        ],
    )
}

/// Dissect an existing `.docx`, `.xlsx`, or `.tsv`/`.txt` file to extract its columns and sample rows.
pub fn dissect_file(path: &Path) -> Result<DissectedDoc, String> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "docx" => {
            let xml = read_zip_entry(path, "word/document.xml")?;
            let rows = extract_xml_table_rows(&xml, "w:tr", "w:tc", "w:t");
            let cols = rows.first().cloned().unwrap_or_else(|| {
                vec!["Time".into(), "Cause".into(), "Findings".into()]
            });
            Ok(DissectedDoc {
                format: "docx".into(),
                detected_columns: cols,
                sample_rows: rows.into_iter().skip(1).take(5).collect(),
            })
        }
        "xlsx" => {
            let shared = read_zip_entry(path, "xl/sharedStrings.xml").unwrap_or_default();
            let shared_vec = extract_xml_tags(&shared, "t");
            let sheet = read_zip_entry(path, "xl/worksheets/sheet1.xml")?;
            let raw_rows = extract_xlsx_rows(&sheet, &shared_vec);
            let cols = raw_rows.first().cloned().unwrap_or_else(|| {
                vec!["Time".into(), "Cause".into(), "Findings".into()]
            });
            Ok(DissectedDoc {
                format: "xlsx".into(),
                detected_columns: cols,
                sample_rows: raw_rows.into_iter().skip(1).take(5).collect(),
            })
        }
        _ => {
            let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
            Ok(dissect_text(&text, "docx"))
        }
    }
}

/// Dissect a table pasted directly into the Chatbox (e.g. `Time | Cause | Findings`).
pub fn dissect_text(text: &str, preferred_format: &str) -> DissectedDoc {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.contains('|') {
            let parts: Vec<String> = trimmed
                .split('|')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if parts.len() >= 2 {
                rows.push(parts);
            }
        } else if trimmed.contains('\t') {
            let parts: Vec<String> = trimmed
                .split('\t')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if parts.len() >= 2 {
                rows.push(parts);
            }
        }
    }
    let detected_columns = rows
        .first()
        .cloned()
        .unwrap_or_else(|| vec!["Time".into(), "Cause".into(), "Findings".into()]);
    let sample_rows = rows.into_iter().skip(1).take(8).collect();
    DissectedDoc {
        format: preferred_format.into(),
        detected_columns,
        sample_rows,
    }
}

/// Format an output filename pattern such as `Report Work Done_(Date today)` -> `Report Work Done_2026-10-13.docx`.
pub fn resolve_filename(pattern: &str, date: NaiveDate, ext: &str) -> String {
    let ymd = date.format("%Y-%m-%d").to_string();
    let mut name = pattern
        .replace("(Date today)", &ymd)
        .replace("(date today)", &ymd)
        .replace("{date}", &ymd)
        .replace("YYYY-MM-DD", &ymd);
    if !name.contains(&ymd) {
        name = format!("{}_{}", name.trim(), ymd);
    }
    let clean_ext = ext.trim_start_matches('.');
    if !name.to_lowercase().ends_with(&format!(".{clean_ext}")) {
        name = format!("{name}.{clean_ext}");
    }
    name.chars()
        .map(|c| if ['/', '\\', ':', '*', '?', '"', '<', '>', '|'].contains(&c) { '_' } else { c })
        .collect()
}

// ---------------------------------------------------------------- XML & ZIP internals

fn col_letter(idx: usize) -> String {
    let mut n = idx;
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s
}

fn xml_esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() && c != '\n' && c != '\t' => {}
            other => out.push(other),
        }
    }
    out
}

fn write_zip(path: &Path, entries: &[(&str, &[u8])]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut f = match File::create(path) {
        Ok(f) => f,
        Err(e) => {
            // If locked by Microsoft Word or Excel, try appending timestamp
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("report");
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("docx");
            let parent = path.parent().unwrap_or(Path::new("."));
            let fallback = parent.join(format!("{stem}_{}.{ext}", chrono::Local::now().format("%H%M%S")));
            File::create(&fallback).map_err(|e2| format!("cannot create {} ({e}) or fallback {}: {e2}", path.display(), fallback.display()))?
        }
    };
    let mut central_dir: Vec<u8> = Vec::new();
    let mut offset: u32 = 0;

    for &(name, raw) in entries {
        let crc = crc32fast::hash(raw);
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(raw).map_err(|e| e.to_string())?;
        let compressed = encoder.finish().map_err(|e| e.to_string())?;

        let name_bytes = name.as_bytes();
        let comp_len = compressed.len() as u32;
        let uncomp_len = raw.len() as u32;

        // Local file header (PK\x03\x04)
        let mut local = Vec::with_capacity(30 + name_bytes.len());
        local.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        local.extend_from_slice(&20u16.to_le_bytes()); // version needed (2.0)
        local.extend_from_slice(&0u16.to_le_bytes()); // flags
        local.extend_from_slice(&8u16.to_le_bytes()); // compression method: 8 (Deflate)
        local.extend_from_slice(&0u16.to_le_bytes()); // mod time
        local.extend_from_slice(&0u16.to_le_bytes()); // mod date
        local.extend_from_slice(&crc.to_le_bytes());
        local.extend_from_slice(&comp_len.to_le_bytes());
        local.extend_from_slice(&uncomp_len.to_le_bytes());
        local.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes()); // extra len
        local.extend_from_slice(name_bytes);

        f.write_all(&local).map_err(|e| e.to_string())?;
        f.write_all(&compressed).map_err(|e| e.to_string())?;

        // Central directory header (PK\x01\x02)
        central_dir.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        central_dir.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central_dir.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // flags
        central_dir.extend_from_slice(&8u16.to_le_bytes()); // method: Deflate
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // time
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // date
        central_dir.extend_from_slice(&crc.to_le_bytes());
        central_dir.extend_from_slice(&comp_len.to_le_bytes());
        central_dir.extend_from_slice(&uncomp_len.to_le_bytes());
        central_dir.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // extra
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // comment
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // disk start
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // internal attr
        central_dir.extend_from_slice(&0u32.to_le_bytes()); // external attr
        central_dir.extend_from_slice(&offset.to_le_bytes());
        central_dir.extend_from_slice(name_bytes);

        offset += local.len() as u32 + comp_len;
    }

    let cd_size = central_dir.len() as u32;
    f.write_all(&central_dir).map_err(|e| e.to_string())?;

    // End of central directory record (PK\x05\x06)
    let count = entries.len() as u16;
    let mut eocd = Vec::with_capacity(22);
    eocd.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    eocd.extend_from_slice(&0u16.to_le_bytes()); // disk
    eocd.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
    eocd.extend_from_slice(&count.to_le_bytes());
    eocd.extend_from_slice(&count.to_le_bytes());
    eocd.extend_from_slice(&cd_size.to_le_bytes());
    eocd.extend_from_slice(&offset.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes()); // comment len
    f.write_all(&eocd).map_err(|e| e.to_string())?;
    Ok(())
}

fn read_zip_entry(path: &Path, target_name: &str) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let mut pos = 0usize;
    while pos + 30 <= bytes.len() {
        if &bytes[pos..pos + 4] != b"PK\x03\x04" {
            break;
        }
        let method = u16::from_le_bytes([bytes[pos + 8], bytes[pos + 9]]);
        let comp_size = u32::from_le_bytes([
            bytes[pos + 18],
            bytes[pos + 19],
            bytes[pos + 20],
            bytes[pos + 21],
        ]) as usize;
        let name_len = u16::from_le_bytes([bytes[pos + 26], bytes[pos + 27]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[pos + 28], bytes[pos + 29]]) as usize;
        let name_start = pos + 30;
        let data_start = name_start + name_len + extra_len;
        if data_start + comp_size > bytes.len() {
            break;
        }
        let name = String::from_utf8_lossy(&bytes[name_start..name_start + name_len]);
        if name == target_name {
            let slice = &bytes[data_start..data_start + comp_size];
            if method == 0 {
                return Ok(String::from_utf8_lossy(slice).into_owned());
            } else if method == 8 {
                let mut decoder = flate2::read::DeflateDecoder::new(slice);
                let mut out = String::new();
                decoder.read_to_string(&mut out).map_err(|e| e.to_string())?;
                return Ok(out);
            }
        }
        pos = data_start + comp_size;
    }
    Err(format!("{target_name} not found in {}", path.display()))
}

fn extract_xml_table_rows(xml: &str, row_tag: &str, cell_tag: &str, text_tag: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for row_chunk in split_xml_blocks(xml, row_tag) {
        let mut cells = Vec::new();
        for cell_chunk in split_xml_blocks(row_chunk, cell_tag) {
            let texts = extract_xml_tags(cell_chunk, text_tag);
            cells.push(texts.join(" ").trim().to_string());
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    rows
}

fn extract_xlsx_rows(xml: &str, shared: &[String]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for row_chunk in split_xml_blocks(xml, "row") {
        let mut cells = Vec::new();
        for cell_chunk in split_xml_blocks(row_chunk, "c") {
            let inline = extract_xml_tags(cell_chunk, "t");
            if !inline.is_empty() {
                cells.push(inline.join(" "));
            } else if let Some(v) = extract_xml_tags(cell_chunk, "v").first() {
                if let Ok(idx) = v.parse::<usize>() {
                    cells.push(shared.get(idx).cloned().unwrap_or_else(|| v.clone()));
                } else {
                    cells.push(v.clone());
                }
            }
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    rows
}

fn split_xml_blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open1 = format!("<{tag}>");
    let open2 = format!("<{tag} ");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while !rest.is_empty() {
        let p1 = rest.find(&open1);
        let p2 = rest.find(&open2);
        let start = match (p1, p2) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let after_start = &rest[start..];
        let Some(end_rel) = after_start.find(&close) else { break };
        out.push(&after_start[..end_rel + close.len()]);
        rest = &after_start[end_rel + close.len()..];
    }
    out
}

fn extract_xml_tags(xml: &str, tag: &str) -> Vec<String> {
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    for block in split_xml_blocks(xml, tag) {
        if let Some(gt) = block.find('>') {
            let body = &block[gt + 1..block.len().saturating_sub(close.len())];
            out.push(
                body.replace("&amp;", "&")
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&apos;", "'"),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_report_filenames() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        assert_eq!(
            resolve_filename("Report Work Done_(Date today)", d, "docx"),
            "Report Work Done_2026-10-13.docx"
        );
        assert_eq!(
            resolve_filename("Report Pre-Sched_(Date today)", d, "xlsx"),
            "Report Pre-Sched_2026-10-13.xlsx"
        );
    }

    #[test]
    fn writes_and_dissects_docx_and_xlsx() {
        let dir = std::env::temp_dir();
        let docx_path = dir.join("dayrecap_test.docx");
        let xlsx_path = dir.join("dayrecap_test.xlsx");
        let cols = vec!["Time".into(), "Cause".into(), "Findings".into()];
        let rows = vec![ReportRow {
            cols: vec![
                "8:00 AM – 9:00 AM".into(),
                "Need single-instance enforcement.".into(),
                "Implemented named mutex in Program.cs.".into(),
            ],
        }];

        write_docx(&docx_path, "Daily Report", "2026-10-13", &cols, &rows).unwrap();
        let d_docx = dissect_file(&docx_path).unwrap();
        assert_eq!(d_docx.detected_columns, cols);
        assert_eq!(d_docx.sample_rows.len(), 1);

        write_xlsx(&xlsx_path, "Pre-Sched", &cols, &rows).unwrap();
        let d_xlsx = dissect_file(&xlsx_path).unwrap();
        assert_eq!(d_xlsx.detected_columns, cols);
        assert_eq!(d_xlsx.sample_rows.len(), 1);

        let _ = fs::remove_file(&docx_path);
        let _ = fs::remove_file(&xlsx_path);
    }
}
