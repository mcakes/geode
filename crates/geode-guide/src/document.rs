//! The bundled guide, divided at Markdown headings without copying its prose.

use std::collections::HashMap;
use std::sync::OnceLock;

use gpui::SharedString;
use markdown::{self as markdown_ast, ParseOptions, mdast::Node};

pub(crate) const SOURCE: &str = include_str!("../../../docs/user-guide.md");

pub(crate) struct Section {
    pub anchor: SharedString,
    pub title: SharedString,
    pub markdown: String,
    pub document: crate::search::Document,
    pub chapter: bool,
}

pub(crate) fn sections() -> &'static [Section] {
    static SECTIONS: OnceLock<Vec<Section>> = OnceLock::new();
    SECTIONS.get_or_init(|| parse(SOURCE))
}

fn slug(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect()
}

fn parse(source: &str) -> Vec<Section> {
    let ast = markdown_ast::to_mdast(source, &ParseOptions::gfm())
        .expect("the bundled user guide is valid Markdown");
    let mut seen = HashMap::<String, usize>::new();
    let headings: Vec<_> = ast
        .children()
        .into_iter()
        .flatten()
        .filter_map(|node| match node {
            Node::Heading(heading) => {
                let title = node.to_string();
                let base = slug(&title);
                let occurrence = seen.entry(base.clone()).or_default();
                let anchor = if *occurrence == 0 {
                    base
                } else {
                    format!("{base}-{occurrence}")
                };
                *occurrence += 1;
                Some((
                    heading.position.as_ref().unwrap().start.offset,
                    title,
                    anchor,
                    heading.depth,
                ))
            }
            _ => None,
        })
        .collect();
    headings
        .iter()
        .enumerate()
        .map(|(ix, (start, title, anchor, depth))| {
            let end = headings.get(ix + 1).map_or(source.len(), |h| h.0);
            let markdown = repository_references_as_text(&source[*start..end]);
            Section {
                anchor: anchor.clone().into(),
                title: title.clone().into(),
                document: crate::search::Document::new(&markdown),
                markdown,
                chapter: *depth <= 2,
            }
        })
        .collect()
}

// Relative repository documents are not shipped as navigable destinations.
// Keep their labels and paths readable instead of handing a relative URL to
// the operating system. Fragment links stay interactive within the guide.
fn repository_references_as_text(source: &str) -> String {
    fn collect(node: &Node, edits: &mut Vec<(std::ops::Range<usize>, String)>) {
        if let Node::Link(link) = node
            && !link.url.starts_with('#')
            && !link.url.contains("://")
        {
            let pos = link.position.as_ref().unwrap();
            edits.push((
                pos.start.offset..pos.end.offset,
                format!("{} (`{}` in the repository)", node.to_string(), link.url),
            ));
            return;
        }
        for child in node.children().into_iter().flatten() {
            collect(child, edits);
        }
    }
    let ast = markdown_ast::to_mdast(source, &ParseOptions::gfm()).unwrap();
    let mut edits = Vec::new();
    collect(&ast, &mut edits);
    let mut text = source.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        text.replace_range(range, &replacement);
    }
    text
}

pub(crate) fn section_for(anchor: &str) -> Option<usize> {
    sections().iter().position(|s| s.anchor.as_ref() == anchor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_inside_code_do_not_split_the_guide() {
        let source = "# Intro\n\n```text\n## Not a section\n```\n\n## Reader's guide\n\nBody\n\n## Reader's guide\n";
        let parts = parse(source);
        assert_eq!(parts.len(), 3);
        assert!(parts[0].markdown.contains("## Not a section"));
        assert_eq!(parts[1].anchor.as_ref(), "readers-guide");
        assert_eq!(parts[2].anchor.as_ref(), "readers-guide-1");
        assert!(parts[1].markdown.contains("Body"));
    }

    #[test]
    fn every_bundled_fragment_link_has_a_section() {
        fn check(node: &Node) {
            if let Node::Link(link) = node
                && let Some(anchor) = link.url.strip_prefix('#')
            {
                assert!(section_for(anchor).is_some(), "missing {anchor}");
            }
            for child in node.children().into_iter().flatten() {
                check(child);
            }
        }
        let ast = markdown_ast::to_mdast(SOURCE, &ParseOptions::gfm()).unwrap();
        check(&ast);
        assert!(sections().len() > 10);
        assert_eq!(sections().iter().filter(|s| s.chapter).count(), 7);
    }

    #[test]
    fn repository_references_remain_readable_without_broken_links() {
        assert_eq!(
            repository_references_as_text("[Guide](#guide) [Setup](../README.md)"),
            "[Guide](#guide) Setup (`../README.md` in the repository)"
        );
    }
}
