//! Search the text people read, then mark those same byte ranges in rich text.
//! Links, formatting delimiters and HTML entities are never searchable syntax.

use std::ops::Range;

use html5ever::tendril::TendrilSink as _;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use regex::{Regex, RegexBuilder};

enum Part {
    Tag(String),
    Text { span: Range<usize>, code: bool },
}

struct Block {
    parts: Vec<Part>,
    text: String,
}

pub(crate) struct Document(Vec<Block>);

pub(crate) struct Marked {
    pub source: String,
    pub count: usize,
    pub first_block: Option<usize>,
    pub block_count: usize,
}

pub(crate) fn query(text: &str) -> Option<Regex> {
    let text = text.trim();
    (!text.is_empty()).then(|| {
        RegexBuilder::new(&regex::escape(text))
            .case_insensitive(true)
            .build()
            .expect("a literal guide query is valid")
    })
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

impl Block {
    fn collect(&mut self, node: &Handle, pre: bool, code: bool) {
        match &node.data {
            NodeData::Text { contents } => {
                let value = contents.borrow();
                // Markdown soft line breaks reflow as spaces. Preserve code's
                // line endings; the component treats them as hard breaks.
                let text = if pre {
                    value.to_string()
                } else {
                    value.replace(['\n', '\r'], " ")
                };
                let start = self.text.len();
                self.text.push_str(&text);
                self.parts.push(Part::Text {
                    span: start..self.text.len(),
                    code,
                });
            }
            NodeData::Element { name, attrs, .. } => {
                let tag = name.local.as_ref();
                if tag == "pre" {
                    fn code_text(node: &Handle, text: &mut String) {
                        if let NodeData::Text { contents } = &node.data {
                            text.push_str(&contents.borrow());
                        }
                        for child in node.children.borrow().iter() {
                            code_text(child, text);
                        }
                    }
                    let mut text = String::new();
                    code_text(node, &mut text);
                    // The component's HTML minifier collapses newlines inside
                    // <pre><code>. Explicit paragraphs keep command examples
                    // on separate selectable lines, with the same mark path
                    // as inline code and without a custom text renderer.
                    self.parts.push(Part::Tag("<div>".into()));
                    for line in text.lines() {
                        self.parts.push(Part::Tag("<p><code>".into()));
                        let start = self.text.len();
                        self.text.push_str(line);
                        self.parts.push(Part::Text {
                            span: start..self.text.len(),
                            code: true,
                        });
                        self.text.push('\n');
                        self.parts.push(Part::Tag("</code></p>".into()));
                    }
                    self.parts.push(Part::Tag("</div>".into()));
                    return;
                }
                let wrapper = matches!(tag, "html" | "head" | "body");
                let boundary = matches!(tag, "li" | "td" | "th" | "p" | "br");
                if boundary {
                    self.text.push('\n');
                }
                if !wrapper {
                    let mut opening = format!("<{tag}");
                    for attr in attrs.borrow().iter() {
                        opening.push(' ');
                        opening.push_str(attr.name.local.as_ref());
                        opening.push_str("=\"");
                        escape(&attr.value, &mut opening);
                        opening.push('"');
                    }
                    opening.push('>');
                    self.parts.push(Part::Tag(opening));
                }
                for child in node.children.borrow().iter() {
                    self.collect(child, pre || tag == "pre", code || tag == "code");
                }
                if !wrapper && !matches!(tag, "br" | "hr" | "img" | "input") {
                    self.parts.push(Part::Tag(format!("</{tag}>")));
                }
                if boundary {
                    self.text.push('\n');
                }
            }
            _ => {
                for child in node.children.borrow().iter() {
                    self.collect(child, pre, code);
                }
            }
        }
    }

    fn render(&self, ranges: &[Range<usize>], color: &str, out: &mut String) {
        for part in &self.parts {
            match part {
                Part::Tag(tag) => out.push_str(tag),
                Part::Text { span, code } => {
                    let mut at = span.start;
                    for range in ranges {
                        let start = range.start.max(span.start);
                        let end = range.end.min(span.end);
                        if start >= end {
                            continue;
                        }
                        escape(&self.text[at..start], out);
                        // The component applies an enclosing code background
                        // after its child marks. Put the search mark outside
                        // each matched code run so both style layers survive.
                        if *code {
                            out.push_str("</code>");
                        }
                        out.push_str("<mark color=\"");
                        out.push_str(color);
                        out.push_str("\">");
                        if *code {
                            out.push_str("<code>");
                        }
                        escape(&self.text[start..end], out);
                        if *code {
                            out.push_str("</code>");
                        }
                        out.push_str("</mark>");
                        if *code {
                            out.push_str("<code>");
                        }
                        at = end;
                    }
                    escape(&self.text[at..span.end], out);
                }
            }
        }
    }
}

impl Document {
    pub fn new(markdown: &str) -> Self {
        fn body(node: &Handle) -> Option<Handle> {
            if matches!(&node.data, NodeData::Element { name, .. } if name.local.as_ref() == "body")
            {
                return Some(node.clone());
            }
            node.children.borrow().iter().find_map(body)
        }
        let html = markdown::to_html_with_options(markdown, &markdown::Options::gfm()).unwrap();
        let dom = html5ever::parse_document(RcDom::default(), Default::default()).one(html);
        let body = body(&dom.document).expect("HTML parser supplies the document body");
        let blocks = body
            .children
            .borrow()
            .iter()
            .filter(|node| matches!(node.data, NodeData::Element { .. }))
            .map(|node| {
                let mut block = Block {
                    parts: Vec::new(),
                    text: String::new(),
                };
                block.collect(node, false, false);
                block
            })
            .collect();
        Self(blocks)
    }

    pub fn contains(&self, query: &Regex) -> bool {
        self.0.iter().any(|block| query.is_match(&block.text))
    }

    pub fn render(&self, query: Option<&Regex>, color: &str) -> Marked {
        let mut marked = Marked {
            source: String::new(),
            count: 0,
            first_block: None,
            block_count: self.0.len(),
        };
        for (ix, block) in self.0.iter().enumerate() {
            let ranges: Vec<_> = query
                .into_iter()
                .flat_map(|q| q.find_iter(&block.text).map(|m| m.range()))
                .collect();
            if !ranges.is_empty() {
                marked.first_block.get_or_insert(ix);
                marked.count += ranges.len();
            }
            block.render(&ranges, color, &mut marked.source);
            // Separate top-level HTML blocks so the Markdown component keeps
            // one virtual-list item per block, including the find scroll target.
            marked.source.push_str("\n\n");
        }
        marked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_visible_text_across_formatting_and_entities_without_touching_links() {
        let doc = Document::new(
            "# Guide\n\nA **book** &amp; `scope` and [book](#hidden-book).\n\n```text\nBOOK\n```\n",
        );
        let marked = doc.render(query("book & scope").as_ref(), "theme-color");
        assert_eq!(marked.count, 1);
        assert_eq!(marked.first_block, Some(1));
        assert!(
            marked
                .source
                .contains("<strong><mark color=\"theme-color\">book</mark></strong>")
        );
        assert!(
            marked
                .source
                .contains("<mark color=\"theme-color\"><code>scope</code></mark>")
        );
        assert!(marked.source.contains("href=\"#hidden-book\""));
        assert!(!doc.contains(&query("hidden-book").unwrap()));
        assert_eq!(doc.render(query("BOOK").as_ref(), "theme-color").count, 3);
        assert!(!doc.render(None, "theme-color").source.contains("<mark"));
    }

    #[test]
    fn unicode_and_literal_queries_preserve_original_byte_ranges() {
        let doc = Document::new("# Guide\n\nÉquité and équité, [x] and <unsafe>.\n");
        let marked = doc.render(query("ÉQUITÉ").as_ref(), "theme-color");
        assert_eq!(marked.count, 2);
        assert!(marked.source.contains(">Équité</mark>"));
        assert!(marked.source.contains(">équité</mark>"));
        assert_eq!(doc.render(query("[x]").as_ref(), "theme-color").count, 1);
        assert!(query("  ").is_none());
    }
}
