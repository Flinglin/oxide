//! Native Mermaid rendering with oxide theme, caching, and GPUI integration.

mod cache;
mod renderer;
mod view;

pub use renderer::MermaidRenderRequest;
pub(crate) use view::MermaidBlock;

pub use cache::{
    RenderedMermaidImage, render_mermaid_svg, render_mermaid_svg_image, render_mermaid_svg_scaled,
};

/// Return true when a fenced code block should be treated as Mermaid.
pub fn is_mermaid_language(language: Option<&str>) -> bool {
    matches!(
        language
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "mermaid" | "mmd"
    )
}

/// Return true when an unlabeled/text code fence is likely a Mermaid diagram.
pub fn is_mermaid_source_candidate(source: &str) -> bool {
    let mut frontmatter = false;
    for line in source.lines().map(str::trim) {
        if line == "---" {
            frontmatter = !frontmatter;
            continue;
        }
        if frontmatter || line.is_empty() || line.starts_with("%%") {
            continue;
        }
        let kind = line
            .split(|character: char| character.is_whitespace() || character == ';')
            .next()
            .unwrap_or_default();
        return matches!(
            kind.to_ascii_lowercase().as_str(),
            "graph"
                | "flowchart"
                | "sequencediagram"
                | "pie"
                | "gantt"
                | "classdiagram"
                | "statediagram"
                | "statediagram-v2"
                | "erdiagram"
                | "mindmap"
                | "journey"
                | "timeline"
                | "requirementdiagram"
                | "gitgraph"
                | "quadrantchart"
                | "xychart-beta"
                | "xychart"
                | "block-beta"
                | "block"
                | "sankey-beta"
                | "sankey"
                | "packet-beta"
                | "packet"
                | "kanban"
                | "architecture-beta"
                | "architecture"
                | "radar-beta"
                | "radar"
                | "treemap-beta"
                | "treemap"
                | "zenuml"
                | "c4context"
                | "c4container"
                | "c4component"
                | "c4dynamic"
                | "c4deployment"
        );
    }
    false
}
