use quick_xml::Reader;
use quick_xml::events::{BytesEnd, BytesStart, Event};

use crate::error::{Result, ZoteroMcpError};
use crate::models::{
    PaperFigure, PaperMetadata, PaperReference, PaperSection, PaperStructure, PaperStructureSource,
};

pub fn parse_tei(item_key: &str, attachment_key: &str, xml: &str) -> Result<PaperStructure> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut state = ParseState::default();
    let mut buf = Vec::new();

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| ZoteroMcpError::Serde(format!("TEI parse error: {e}")))?
        {
            Event::Start(e) => state.handle_start(&e),
            Event::End(e) => {
                let name = local_name_end(&e);
                state.handle_end_name(&name);
            }
            Event::Empty(e) => {
                let name = local_name(&e);
                state.handle_start(&e);
                state.handle_end_name(&name);
            }
            Event::Text(e) => {
                let text = e
                    .unescape()
                    .map_err(|err| ZoteroMcpError::Serde(format!("TEI text decode: {err}")))?
                    .into_owned();
                state.handle_text(&text);
            }
            Event::CData(e) => {
                let text = String::from_utf8_lossy(&e.into_inner()).into_owned();
                state.handle_text(&text);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(build_structure(item_key, attachment_key, state))
}

#[derive(Default)]
struct ParseState {
    stack: Vec<String>,

    // header
    title: Option<String>,
    title_capturing: bool,
    title_buf: String,

    doi: Option<String>,
    doi_capturing: bool,
    doi_buf: String,

    year: Option<String>,

    abstract_buf: String,
    abstract_capturing: bool,

    authors: Vec<String>,
    in_analytic: bool,
    in_back_bibl: bool,
    persname_forenames: Vec<String>,
    persname_surname: Option<String>,
    in_persname: bool,
    forename_capturing: bool,
    forename_buf: String,
    surname_capturing: bool,
    surname_buf: String,

    // body
    in_body: bool,
    current_section: Option<PendingSection>,
    sections: Vec<PaperSection>,

    // references
    in_list_bibl: bool,
    current_ref: Option<PendingRef>,
    references: Vec<PaperReference>,

    // figures
    in_figure: bool,
    figure_label_capturing: bool,
    figure_label_buf: String,
    figure_caption_capturing: bool,
    figure_caption_buf: String,
    pending_figure_label: Option<String>,
    pending_figure_caption: Option<String>,
    figures: Vec<PaperFigure>,
}

struct PendingSection {
    level: u8,
    heading: Option<String>,
    heading_capturing: bool,
    heading_buf: String,
    text_buf: String,
}

struct PendingRef {
    raw_buf: String,
    title: Option<String>,
    title_capturing: bool,
    title_buf: String,
    authors: Vec<String>,
    year: Option<String>,
    doi: Option<String>,
    doi_capturing: bool,
    doi_buf: String,
}

impl ParseState {
    fn handle_start(&mut self, e: &BytesStart<'_>) {
        let name = local_name(e);
        match name.as_str() {
            "title" => {
                let is_main =
                    attr_eq(e, "type", "main") || attr_eq(e, "level", "a") && self.title.is_none();
                if self.title.is_none() && is_main && !self.in_list_bibl {
                    self.title_capturing = true;
                    self.title_buf.clear();
                }
                // Ref title inside biblStruct
                if let Some(pref) = self.current_ref.as_mut()
                    && pref.title.is_none()
                {
                    pref.title_capturing = true;
                    pref.title_buf.clear();
                }
            }
            "idno" => {
                let is_doi = attr_eq(e, "type", "DOI") || attr_eq(e, "type", "doi");
                if is_doi {
                    if let Some(pref) = self.current_ref.as_mut() {
                        pref.doi_capturing = true;
                        pref.doi_buf.clear();
                    } else if self.doi.is_none() {
                        self.doi_capturing = true;
                        self.doi_buf.clear();
                    }
                }
            }
            "date" => {
                if (attr_eq(e, "type", "published") || self.current_ref.is_some())
                    && let Some(when) = attr_value(e, "when")
                {
                    let year = when.chars().take(4).collect::<String>();
                    if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) {
                        if let Some(pref) = self.current_ref.as_mut() {
                            if pref.year.is_none() {
                                pref.year = Some(year);
                            }
                        } else if self.year.is_none() {
                            self.year = Some(year);
                        }
                    }
                }
            }
            "abstract" => {
                self.abstract_capturing = true;
            }
            "analytic" => {
                if !self.in_back_bibl {
                    self.in_analytic = true;
                }
            }
            "listBibl" => {
                self.in_list_bibl = true;
                self.in_back_bibl = true;
            }
            "biblStruct" if self.in_list_bibl => {
                self.current_ref = Some(PendingRef {
                    raw_buf: String::new(),
                    title: None,
                    title_capturing: false,
                    title_buf: String::new(),
                    authors: Vec::new(),
                    year: None,
                    doi: None,
                    doi_capturing: false,
                    doi_buf: String::new(),
                });
            }
            "persName" => {
                self.in_persname = true;
                self.persname_forenames.clear();
                self.persname_surname = None;
            }
            "forename" if self.in_persname => {
                self.forename_capturing = true;
                self.forename_buf.clear();
            }
            "surname" if self.in_persname => {
                self.surname_capturing = true;
                self.surname_buf.clear();
            }
            "body" => {
                self.in_body = true;
            }
            "div" if self.in_body && self.current_section.is_none() => {
                self.finalize_section();
                self.current_section = Some(PendingSection {
                    level: 1,
                    heading: None,
                    heading_capturing: false,
                    heading_buf: String::new(),
                    text_buf: String::new(),
                });
            }
            "head" => {
                if let Some(sec) = self.current_section.as_mut() {
                    if sec.heading.is_none() {
                        sec.heading_capturing = true;
                        sec.heading_buf.clear();
                    }
                    if let Some(n) = attr_value(e, "n") {
                        let level = (n.matches('.').count() as u8).saturating_add(1);
                        sec.level = level.max(1);
                    }
                }
            }
            "figure" => {
                self.in_figure = true;
                self.pending_figure_label = None;
                self.pending_figure_caption = None;
            }
            "label" if self.in_figure => {
                self.figure_label_capturing = true;
                self.figure_label_buf.clear();
            }
            "figDesc" if self.in_figure => {
                self.figure_caption_capturing = true;
                self.figure_caption_buf.clear();
            }
            _ => {}
        }
        self.stack.push(name);
    }

    fn handle_end_name(&mut self, name: &str) {
        match name {
            "title" => {
                if self.title_capturing {
                    self.title = Some(self.title_buf.trim().to_string());
                    self.title_capturing = false;
                }
                if let Some(pref) = self.current_ref.as_mut()
                    && pref.title_capturing
                {
                    pref.title = Some(pref.title_buf.trim().to_string());
                    pref.title_capturing = false;
                }
            }
            "idno" => {
                if self.doi_capturing {
                    self.doi = Some(self.doi_buf.trim().to_string());
                    self.doi_capturing = false;
                }
                if let Some(pref) = self.current_ref.as_mut()
                    && pref.doi_capturing
                {
                    pref.doi = Some(pref.doi_buf.trim().to_string());
                    pref.doi_capturing = false;
                }
            }
            "abstract" => {
                self.abstract_capturing = false;
            }
            "analytic" => {
                self.in_analytic = false;
            }
            "listBibl" => {
                self.in_list_bibl = false;
                self.in_back_bibl = false;
            }
            "biblStruct" => {
                if let Some(pref) = self.current_ref.take() {
                    let raw = collapse_ws(&pref.raw_buf);
                    let id = format!("b{}", self.references.len());
                    self.references.push(PaperReference {
                        id,
                        raw,
                        authors: pref.authors,
                        title: pref.title,
                        year: pref.year,
                        doi: pref.doi,
                    });
                }
            }
            "persName" => {
                if self.in_persname {
                    let forename = self.persname_forenames.join(" ");
                    let full = match (&forename.trim().is_empty(), &self.persname_surname) {
                        (false, Some(last)) => format!("{} {}", forename.trim(), last.trim()),
                        (true, Some(last)) => last.trim().to_string(),
                        (false, None) => forename.trim().to_string(),
                        (true, None) => String::new(),
                    };
                    if !full.is_empty() {
                        if let Some(pref) = self.current_ref.as_mut() {
                            pref.authors.push(full);
                        } else if self.in_analytic {
                            self.authors.push(full);
                        }
                    }
                }
                self.in_persname = false;
                self.persname_forenames.clear();
                self.persname_surname = None;
            }
            "forename" => {
                if self.forename_capturing {
                    let v = self.forename_buf.trim().to_string();
                    if !v.is_empty() {
                        self.persname_forenames.push(v);
                    }
                    self.forename_capturing = false;
                }
            }
            "surname" => {
                if self.surname_capturing {
                    let v = self.surname_buf.trim().to_string();
                    if !v.is_empty() {
                        self.persname_surname = Some(v);
                    }
                    self.surname_capturing = false;
                }
            }
            "head" => {
                if let Some(sec) = self.current_section.as_mut()
                    && sec.heading_capturing
                {
                    sec.heading = Some(sec.heading_buf.trim().to_string());
                    sec.heading_capturing = false;
                }
            }
            "div" => {
                if self.current_section.is_some()
                    && self.stack.iter().rev().skip(1).any(|t| t == "body")
                    && !self
                        .stack
                        .iter()
                        .rev()
                        .skip(1)
                        .take_while(|t| *t != "body")
                        .any(|t| t == "div")
                {
                    self.finalize_section();
                }
            }
            "body" => {
                self.finalize_section();
                self.in_body = false;
            }
            "label" => {
                if self.figure_label_capturing {
                    self.pending_figure_label = Some(self.figure_label_buf.trim().to_string());
                    self.figure_label_capturing = false;
                }
            }
            "figDesc" => {
                if self.figure_caption_capturing {
                    self.pending_figure_caption = Some(self.figure_caption_buf.trim().to_string());
                    self.figure_caption_capturing = false;
                }
            }
            "figure" => {
                if self.in_figure {
                    let id = format!("f{}", self.figures.len());
                    self.figures.push(PaperFigure {
                        id,
                        label: self.pending_figure_label.take(),
                        caption: self.pending_figure_caption.take().unwrap_or_default(),
                    });
                }
                self.in_figure = false;
            }
            _ => {}
        }
        self.stack.pop();
    }

    fn handle_text(&mut self, text: &str) {
        if self.title_capturing {
            self.title_buf.push_str(text);
            self.title_buf.push(' ');
        }
        if self.doi_capturing {
            self.doi_buf.push_str(text);
        }
        if self.abstract_capturing {
            if !self.abstract_buf.is_empty() {
                self.abstract_buf.push(' ');
            }
            self.abstract_buf.push_str(text);
        }
        if self.forename_capturing {
            self.forename_buf.push_str(text);
        }
        if self.surname_capturing {
            self.surname_buf.push_str(text);
        }
        if let Some(sec) = self.current_section.as_mut() {
            if sec.heading_capturing {
                sec.heading_buf.push_str(text);
                sec.heading_buf.push(' ');
            } else {
                if !sec.text_buf.is_empty() {
                    sec.text_buf.push(' ');
                }
                sec.text_buf.push_str(text);
            }
        }
        if let Some(pref) = self.current_ref.as_mut() {
            if pref.title_capturing {
                pref.title_buf.push_str(text);
                pref.title_buf.push(' ');
            }
            if pref.doi_capturing {
                pref.doi_buf.push_str(text);
            }
            if !pref.raw_buf.is_empty() {
                pref.raw_buf.push(' ');
            }
            pref.raw_buf.push_str(text);
        }
        if self.figure_label_capturing {
            self.figure_label_buf.push_str(text);
        }
        if self.figure_caption_capturing {
            if !self.figure_caption_buf.is_empty() {
                self.figure_caption_buf.push(' ');
            }
            self.figure_caption_buf.push_str(text);
        }
    }

    fn finalize_section(&mut self) {
        if let Some(sec) = self.current_section.take() {
            let heading = sec
                .heading
                .map(|s| collapse_ws(&s))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Untitled".to_string());
            let text = collapse_ws(&sec.text_buf);
            let id = format!("s{}", self.sections.len());
            self.sections.push(PaperSection {
                id,
                heading,
                level: sec.level,
                text,
                subsections: Vec::new(),
            });
        }
    }
}

fn local_name(e: &BytesStart<'_>) -> String {
    let full = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    full.rsplit(':').next().unwrap_or(&full).to_string()
}

fn local_name_end(e: &BytesEnd<'_>) -> String {
    let full = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    full.rsplit(':').next().unwrap_or(&full).to_string()
}

fn attr_value(e: &BytesStart<'_>, key: &str) -> Option<String> {
    for attr in e.attributes().flatten() {
        let k = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let local = k.rsplit(':').next().unwrap_or(&k);
        if local == key {
            return Some(String::from_utf8_lossy(&attr.value).into_owned());
        }
    }
    None
}

fn attr_eq(e: &BytesStart<'_>, key: &str, value: &str) -> bool {
    attr_value(e, key)
        .map(|v| v.eq_ignore_ascii_case(value))
        .unwrap_or(false)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn build_structure(item_key: &str, attachment_key: &str, state: ParseState) -> PaperStructure {
    let metadata = PaperMetadata {
        title: state.title.filter(|s| !s.is_empty()),
        authors: state.authors,
        abstract_note: {
            let s = collapse_ws(&state.abstract_buf);
            if s.is_empty() { None } else { Some(s) }
        },
        doi: state.doi.filter(|s| !s.is_empty()),
        year: state.year,
    };

    PaperStructure {
        item_key: item_key.to_string(),
        attachment_key: Some(attachment_key.to_string()),
        metadata,
        sections: state.sections,
        references: state.references,
        figures: state.figures,
        source: PaperStructureSource::Grobid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TEI: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<TEI xmlns="http://www.tei-c.org/ns/1.0">
  <teiHeader>
    <fileDesc>
      <titleStmt><title level="a" type="main">On Attention Mechanisms</title></titleStmt>
      <sourceDesc>
        <biblStruct>
          <analytic>
            <author><persName><forename type="first">Jane</forename><surname>Doe</surname></persName></author>
            <author><persName><forename type="first">John</forename><surname>Smith</surname></persName></author>
            <idno type="DOI">10.1234/xyz</idno>
          </analytic>
          <monogr><imprint><date type="published" when="2023-06-15">2023</date></imprint></monogr>
        </biblStruct>
      </sourceDesc>
    </fileDesc>
    <profileDesc>
      <abstract><p>This is the abstract.</p></abstract>
    </profileDesc>
  </teiHeader>
  <text>
    <body>
      <div>
        <head n="1">Introduction</head>
        <p>Intro paragraph text.</p>
      </div>
      <div>
        <head n="2.1">Related Work</head>
        <p>Related work paragraph.</p>
      </div>
    </body>
    <back>
      <div type="references">
        <listBibl>
          <biblStruct>
            <analytic>
              <title level="a" type="main">Ref Title One</title>
              <author><persName><forename>A</forename><surname>One</surname></persName></author>
              <idno type="DOI">10.5555/ref1</idno>
            </analytic>
            <monogr><imprint><date when="2019">2019</date></imprint></monogr>
          </biblStruct>
        </listBibl>
      </div>
    </back>
  </text>
</TEI>
"#;

    #[test]
    fn parses_core_metadata_and_sections() {
        let s = parse_tei("ITEM1", "ATT1", SAMPLE_TEI).expect("parse");
        assert_eq!(s.item_key, "ITEM1");
        assert_eq!(s.attachment_key.as_deref(), Some("ATT1"));
        assert_eq!(s.metadata.title.as_deref(), Some("On Attention Mechanisms"));
        assert_eq!(
            s.metadata.authors,
            vec!["Jane Doe".to_string(), "John Smith".to_string()]
        );
        assert_eq!(s.metadata.doi.as_deref(), Some("10.1234/xyz"));
        assert_eq!(s.metadata.year.as_deref(), Some("2023"));
        assert!(
            s.metadata
                .abstract_note
                .as_deref()
                .unwrap()
                .contains("This is the abstract.")
        );
        assert_eq!(s.sections.len(), 2);
        assert_eq!(s.sections[0].heading, "Introduction");
        assert_eq!(s.sections[0].level, 1);
        assert!(s.sections[0].text.contains("Intro paragraph text."));
        assert_eq!(s.sections[1].heading, "Related Work");
        assert_eq!(s.sections[1].level, 2);
    }

    #[test]
    fn parses_references() {
        let s = parse_tei("X", "Y", SAMPLE_TEI).expect("parse");
        assert_eq!(s.references.len(), 1);
        let r = &s.references[0];
        assert_eq!(r.title.as_deref(), Some("Ref Title One"));
        assert_eq!(r.doi.as_deref(), Some("10.5555/ref1"));
        assert_eq!(r.year.as_deref(), Some("2019"));
        assert_eq!(r.authors, vec!["A One".to_string()]);
    }

    #[test]
    fn source_is_grobid() {
        let s = parse_tei("X", "Y", SAMPLE_TEI).expect("parse");
        assert!(matches!(s.source, PaperStructureSource::Grobid));
    }
}
