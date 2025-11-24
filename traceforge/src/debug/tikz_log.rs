use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Result, Write};
use std::path::Path;
use crate::event::Event;
use crate::event_label::{AsEventLabel, LabelEnum};
use crate::exec_graph::ExecutionGraph;

const GRAPH_NODE_MARKER: &str = "% GRAPH_NODE";
const LEGEND_NODE_MARKER: &str = "% LEGEND_NODE";
const DOC_FOOTER: &str = "\\end{tikzpicture}\n\\end{document}\n";
const H_SPACING_CM: f32 = 10.0;
const LANE_SPACING: f32 = 2.0;
const LANE_OFFSET: f32 = 1.5;
const STEP: f32 = 1.5;
const BEGIN_SHIFT_STEP: f32 = 0.5;

/// Write a TikZ representation of the supplied execution graph to `path`.
/// The output is a standalone document with one outer tikzpicture containing
/// a legend node and one node per graph (each node contains its own tikzpicture).
pub(crate) fn write_tikz_graph(graph: &ExecutionGraph, path: &str, truncate: bool) -> Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    if truncate || !Path::new(path).exists() {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        file.write_all(preamble().as_bytes())?;
        file.write_all(legend_node().as_bytes())?;
        file.write_all(graph_node(graph, 0).as_bytes())?;
        file.write_all(DOC_FOOTER.as_bytes())?;
        return Ok(());
    }

    let mut existing = fs::read_to_string(path)?;
    let trimmed = existing.trim_end();
    if trimmed.ends_with(DOC_FOOTER) {
        let new_len = trimmed.len() - DOC_FOOTER.len();
        existing.truncate(new_len);
    } else if let Some(idx) = trimmed.rfind("\\end{tikzpicture}") {
        existing.truncate(idx);
    }

    let graph_count = existing.matches(GRAPH_NODE_MARKER).count();
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(existing.trim_end().as_bytes())?;
    file.write_all(b"\n")?;
    file.write_all(graph_node(graph, graph_count).as_bytes())?;
    file.write_all(DOC_FOOTER.as_bytes())
}

fn preamble() -> String {
    [
        "\\documentclass[tikz]{standalone}",
        "\\usepackage{xcolor}",
        "\\usepackage{tikz}",
        "\\begin{document}",
        "\\begin{tikzpicture}",
    ]
    .join("\n")
        + "\n"
}

fn legend_node() -> String {
    let mut out = String::new();
    out.push_str(&format!("{LEGEND_NODE_MARKER}\n"));
    out.push_str("\\node[anchor=north west] at (0,0) {\n");
    out.push_str(&legend_picture());
    out.push_str("\n};\n");
    out
}

fn graph_node(graph: &ExecutionGraph, idx: usize) -> String {
    let x = (idx as f32 + 1.0) * H_SPACING_CM;
    let mut out = String::new();
    out.push_str(&format!("{GRAPH_NODE_MARKER} {}\n", idx));
    out.push_str(&format!("\\node[anchor=north west] at ({:.2}cm,0) {{\n", x));
    out.push_str(&graph_picture(graph));
    out.push_str("\n};\n");
    out
}

fn legend_picture() -> String {
    let mut out = String::new();
    out.push_str("\\begin{tikzpicture}\n");
    out.push_str(legend_styles());
    out.push_str(&legend_tikz());
    out.push_str("\\end{tikzpicture}\n");
    out
}

fn graph_picture(graph: &ExecutionGraph) -> String {
    let mut out = String::new();
    out.push_str("\\begin{tikzpicture}[>=stealth, node distance=8mm]\n");
    out.push_str(style_defs());

    // Compute Y offsets per event, adjusting an entire thread when aligning Begin to parent.
    let mut y_positions: HashMap<Event, f32> = HashMap::new();
    for (_lane, tid) in graph.thread_ids().iter().enumerate() {
        let size = graph.thread_size(*tid);
        let mut offset = 0f32;
        for idx in 0..size {
            let pos = Event::new(*tid, idx as u32);
            let lab = graph.label(pos);
            let base_y = -(idx as f32) * STEP + offset;
            let mut y = base_y;
            if let LabelEnum::Begin(blab) = lab {
                if let Some(parent) = blab.parent() {
                    if let Some(&parent_y) = y_positions.get(&parent) {
                        y = parent_y - STEP * BEGIN_SHIFT_STEP;
                        offset += y - base_y;
                    }
                }
            }
            y_positions.insert(pos, y);
        }
    }

    // Nodes
    for (lane, tid) in graph.thread_ids().iter().enumerate() {
        let size = graph.thread_size(*tid);
        let mut prev_node_id: Option<String> = None;
        let mut prev_y: Option<f32> = None;
        for idx in 0..size {
            let pos = Event::new(*tid, idx as u32);
            let lab = graph.label(pos);
            let x = (lane as f32) * LANE_SPACING + LANE_OFFSET;
            let y = *y_positions
                .get(&pos)
                .unwrap_or_else(|| panic!("Missing y position for {:?}", pos));
            let node_id = node_id(&pos);
            let label = format_node_label(lab);
            match (&prev_node_id, prev_y) {
                (Some(prev), Some(prev_y_val)) => {
                    let mut distance = prev_y_val - y;
                    if distance <= 0.0 {
                        distance = STEP;
                    }
                    out.push_str(&format!(
                        "\\node[threadnode, below of={}, node distance={:.2}cm] ({}) {{\\tiny {}}};\n",
                        prev,
                        distance,
                        node_id,
                        label
                    ));
                }
                _ => {
                    out.push_str(&format!(
                        "\\node[threadnode] ({}) at ({:.2}, {:.2}) {{\\tiny {}}};\n",
                        node_id, x, y, label
                    ));
                }
            }
            prev_node_id = Some(node_id);
            prev_y = Some(y);
        }
    }

    // Program order edges (per-thread)
    for tid in graph.thread_ids().iter() {
        let size = graph.thread_size(*tid);
        for idx in 1..size {
            let from = Event::new(*tid, (idx - 1) as u32);
            let to = Event::new(*tid, idx as u32);
            out.push_str(&format!(
                "\\draw[po] ({}) -- ({});\n",
                node_id(&from),
                node_id(&to)
            ));
        }
    }

    // Additional edges: rf, spawn, join
    for tid in graph.thread_ids().iter() {
        let size = graph.thread_size(*tid);
        for idx in 0..size {
            let current = Event::new(*tid, idx as u32);
            let lab = graph.label(current);
            match lab {
                LabelEnum::RecvMsg(rlab) => {
                    if let Some(rf) = rlab.rf() {
                        out.push_str(&format!(
                            "\\draw[rf] ({}) -> ({});\n",
                            node_id(&rf),
                            node_id(&current)
                        ));
                    }
                }
                LabelEnum::Begin(blab) => {
                    if let Some(parent) = blab.parent() {
                        out.push_str(&format!(
                            "\\draw[spawn] ({}) -> ({});\n",
                            node_id(&parent),
                            node_id(&current)
                        ));
                    }
                }
                LabelEnum::TJoin(jlab) => {
                    if let Some(end) = graph.thread_last(jlab.cid()) {
                        out.push_str(&format!(
                            "\\draw[joinedge] ({}) -> ({});\n",
                            node_id(&end.pos()),
                            node_id(&current)
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    out.push_str("\\end{tikzpicture}\n");
    out
}

fn format_label(lab: &LabelEnum) -> String {
    match lab {
        LabelEnum::SendMsg(s) => {
            let target = s
                .reader()
                .or_else(|| s.monitor_readers().first().copied())
                .map(|e| format!("{}", e.thread))
                .unwrap_or_else(|| "?".to_string());
            let pos = s.pos();
            format!("({}, {}): S({})", pos.thread, pos.index, target)
        }
        LabelEnum::RecvMsg(r) => {
            let pos = r.pos();
            format!("({}, {}): R()", pos.thread, pos.index)
        }
        _ => format!("{}", lab),
    }
}

fn format_node_label(lab: &LabelEnum) -> String {
    let label = format_label(lab);
    let (prefix, rest) = split_label_prefix(&label);

    if prefix.is_empty() {
        return tex_escape(&label);
    }

    let escaped_prefix = tex_escape(prefix);
    let escaped_rest = tex_escape(rest);
    /*format!(
        "\\textcolor{{gray}}{{\\tiny {}}}\\;{}",
        escaped_prefix, escaped_rest
    )*/
    format!(
        "{}",
        escaped_rest
    )
}

fn split_label_prefix(label: &str) -> (&str, &str) {
    if let Some(idx) = label.find(':') {
        let (prefix, rest) = label.split_at(idx + 1);
        (prefix.trim(), rest.trim_start())
    } else {
        ("", label)
    }
}

fn style_defs() -> &'static str {
    "\\tikzstyle{threadnode}=[inner sep=1.2pt, align=center];\\tikzstyle{po}=[->, thin];\\tikzstyle{rf}=[->, thick, green!60!black];\\tikzstyle{spawn}=[->, dashed, blue!60!black];\\tikzstyle{joinedge}=[->, dotted, red!70!black];\n"
}

fn legend_styles() -> &'static str {
    "\\tikzstyle{po}=[->, thin];\\tikzstyle{rf}=[->, thick, green!60!black];\\tikzstyle{spawn}=[->, dashed, blue!60!black];\\tikzstyle{joinedge}=[->, dotted, red!70!black];\n"
}

fn legend_tikz() -> String {
    let mut out = String::new();
    out.push_str("\\begin{scope}[yshift=0cm, every node/.style={font=\\scriptsize}]\n");
    out.push_str("\\node[draw, rounded corners, inner sep=2pt] (a1) at (0,0) {event};\n");
    out.push_str("\\node[draw, rounded corners, inner sep=2pt] (a2) at (2,0) {event};\n");
    out.push_str("\\draw[po] (a1) -- (a2);\n");
    out.push_str("\\node at (1,0.3) {po};\n");

    out.push_str("\\node[draw, rounded corners, inner sep=2pt] (b1) at (0,-1) {recv};\n");
    out.push_str("\\node[draw, rounded corners, inner sep=2pt] (b2) at (2,-1) {send};\n");
    out.push_str("\\draw[rf] (b2) -> (b1);\n");
    out.push_str("\\node at (1,-0.7) {rf};\n");

    out.push_str("\\node[draw, rounded corners, inner sep=2pt] (c1) at (0,-2) {parent};\n");
    out.push_str("\\node[draw, rounded corners, inner sep=2pt] (c2) at (2,-2) {begin};\n");
    out.push_str("\\draw[spawn] (c1) -> (c2);\n");
    out.push_str("\\node at (1,-1.7) {spawn};\n");

    out.push_str("\\node[draw, rounded corners, inner sep=2pt] (d1) at (0,-3) {end};\n");
    out.push_str("\\node[draw, rounded corners, inner sep=2pt] (d2) at (2,-3) {join};\n");
    out.push_str("\\draw[joinedge] (d1) -> (d2);\n");
    out.push_str("\\node at (1,-2.7) {join};\n");
    out.push_str("\\end{scope}\n");
    out
}

fn node_id(pos: &Event) -> String {
    format!("t{}i{}", pos.thread, pos.index)
}

fn tex_escape(s: &str) -> String {
    let mut escaped = String::new();
    for ch in s.chars() {
        match ch {
            '\\' => escaped.push_str("\\textbackslash{}"),
            '{' => escaped.push_str("\\{"),
            '}' => escaped.push_str("\\}"),
            '%' => escaped.push_str("\\%"),
            '_' => escaped.push_str("\\_"),
            '$' => escaped.push_str("\\$"),
            '&' => escaped.push_str("\\&"),
            '#' => escaped.push_str("\\#"),
            '^' => escaped.push_str("\\^"),
            '~' => escaped.push_str("\\~{}"),
            '<' => escaped.push_str("$<$"),
            '>' => escaped.push_str("$>$"),
            '|' => escaped.push_str("$|$"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
