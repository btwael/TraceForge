use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Result, Write};
use std::fs::File;
use std::path::PathBuf;
use crate::event::Event;
use crate::event_label::{AsEventLabel, LabelEnum};
use crate::exec_graph::ExecutionGraph;
use crate::BlockType;

const GRAPH_NODE_MARKER: &str = "% GRAPH_NODE";
const DOC_FOOTER: &str = "\\end{tikzpicture}\n\\end{document}\n";
const LANE_SPACING: f32 = 2.0;
const LANE_OFFSET: f32 = 1.5;
const STEP: f32 = 1.0;
const BEGIN_SHIFT_STEP: f32 = 0.5;

/// Write a TikZ representation of the supplied execution graph to `path`.
/// The output is a standalone document with one outer tikzpicture containing
/// a legend node and one node per graph (each node contains its own tikzpicture).
pub(crate) fn write_tikz_graph(
    graph: &ExecutionGraph,
    path: &str,
    truncate: bool,
    row: usize,
    col: usize,
    row_start: usize,
    is_complete: bool,
) -> Result<()> {
    let graph_idx = next_graph_index(path, truncate)?;

    let mut manifest = if truncate && graph_idx == 0 {
        Manifest::default()
    } else {
        load_manifest(path)?
    };
    manifest.graphs.insert(graph_idx, (row, col, is_complete));
    manifest
        .row_starts
        .entry(row)
        .or_insert(row_start);
    save_manifest(path, &manifest)?;

    write_single_graph(graph, path, graph_idx)?;
    write_build_script(path)?;
    write_gallery_html(path)?;

    Ok(())
}

fn preamble() -> String {
    [
        "\\documentclass[tikz]{standalone}",
        "\\usepackage{xcolor}",
        "\\usepackage{tikz}",
        "\\usetikzlibrary{positioning,fit}",
        "\\begin{document}",
        "\\begin{tikzpicture}[node distance=1.5cm]",
    ]
    .join("\n")
        + "\n"
}

fn graph_node_standalone(graph: &ExecutionGraph, idx: usize) -> String {
    let node_name = graph_node_name(0, idx);
    let mut out = String::new();
    out.push_str(&format!("{GRAPH_NODE_MARKER} {}\n", idx));
    out.push_str(&format!(
        "\\node[anchor=north west] ({}) at (0,0) {{\n",
        node_name
    ));
    out.push_str(&graph_picture(graph));
    out.push_str("\n};\n");
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
        LabelEnum::Block(b) => match b.btype() {
            BlockType::Join(tid) => format!("!TJOIN({})", tid),
            BlockType::Value(_) => "!R()".to_string(),
            other => format!("!{:?}", other),
        },
        _ => format!("{}", lab),
    }
}

fn format_node_label(lab: &LabelEnum) -> String {
    let label = format_label(lab);
    if matches!(lab, LabelEnum::Block(_)) {
        return format!("\\textcolor{{gray}}{{{}}}", tex_escape(&label));
    }
    let (prefix, rest) = split_label_prefix(&label);

    if prefix.is_empty() {
        return tex_escape(&label);
    }

    let _escaped_prefix = tex_escape(prefix);
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

fn graph_node_name(row: usize, col: usize) -> String {
    format!("graph_r{row}_c{col}")
}

fn write_single_graph(graph: &ExecutionGraph, base_path: &str, idx: usize) -> Result<()> {
    let Some(target) = single_graph_path(base_path, idx) else {
        return Ok(()); // Nothing to do if we cannot determine a path
    };

    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(target)?;

    file.write_all(preamble().as_bytes())?;
    file.write_all(graph_node_standalone(graph, 0).as_bytes())?;
    file.write_all(DOC_FOOTER.as_bytes())?;
    Ok(())
}

fn single_graph_path(base_path: &str, idx: usize) -> Option<std::path::PathBuf> {
    let (dir, stem) = base_dir_and_stem(base_path);
    let dir = dir.join("graphs");
    let fname = format!("{}_g{:04}.tex", stem, idx);
    Some(dir.join(fname))
}

fn next_graph_index(base_path: &str, truncate: bool) -> Result<usize> {
    let manifest = if truncate {
        Manifest::default()
    } else {
        load_manifest(base_path)?
    };
    Ok(manifest.graphs.keys().max().map(|v| v + 1).unwrap_or(0))
}

fn build_script_path(base_path: &str) -> Option<PathBuf> {
    let (dir, _) = base_dir_and_stem(base_path);
    Some(dir.join("build_graphs.sh"))
}

fn write_build_script(base_path: &str) -> Result<()> {
    let Some(path) = build_script_path(base_path) else {
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    if path.exists() {
        return Ok(()); // don't overwrite user script
    }

    let script = r#"#!/usr/bin/env bash
set -euo pipefail

if ! command -v pdflatex >/dev/null 2>&1; then
  echo "pdflatex not found; please install a LaTeX distribution." >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GRAPH_DIR="${SCRIPT_DIR}/graphs"

if [[ ! -d "${GRAPH_DIR}" ]]; then
  echo "Graph directory '${GRAPH_DIR}' not found." >&2
  exit 1
fi

shopt -s nullglob
cd "${GRAPH_DIR}"
for tex in *.tex; do
  [[ -e "${tex}" ]] || continue
  pdf="${tex%.tex}.pdf"
  png="${tex%.tex}.png"
  pdflatex -interaction=nonstopmode -halt-on-error "${tex}" >/dev/null
  if command -v pdftoppm >/dev/null 2>&1; then
    pdftoppm -png -singlefile "${pdf}" "${tex%.tex}" >/dev/null
  elif command -v convert >/dev/null 2>&1; then
    convert -density 300 "${pdf}" "${png}" >/dev/null
  else
    echo "Neither pdftoppm nor convert found; produced PDF at ${pdf}." >&2
  fi
done
"#;

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path.clone())?;
    file.write_all(script.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn gallery_html_path(base_path: &str) -> Option<PathBuf> {
    let (dir, _) = base_dir_and_stem(base_path);
    Some(dir.join("gallery.html"))
}

fn manifest_path(base_path: &str) -> Option<PathBuf> {
    let (dir, stem) = base_dir_and_stem(base_path);
    Some(dir.join("graphs").join(format!("{}_manifest.txt", stem)))
}

#[derive(Default)]
struct Manifest {
    graphs: HashMap<usize, (usize, usize, bool)>,
    row_starts: HashMap<usize, usize>,
}

fn load_manifest(base_path: &str) -> Result<Manifest> {
    let Some(path) = manifest_path(base_path) else {
        return Ok(Manifest::default());
    };
    if !path.exists() {
        return Ok(Manifest::default());
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut out = Manifest::default();
    for line in reader.lines() {
        if let Ok(l) = line {
            let parts: Vec<_> = l.split_whitespace().collect();
            match parts.as_slice() {
                ["G", idx, row, col, flag] => {
                    if let (Ok(i), Ok(r), Ok(c), Ok(f)) = (
                        idx.parse::<usize>(),
                        row.parse::<usize>(),
                        col.parse::<usize>(),
                        flag.parse::<u8>(),
                    ) {
                        out.graphs.insert(i, (r, c, f != 0));
                    }
                }
                ["G", idx, row, col] => {
                    if let (Ok(i), Ok(r), Ok(c)) =
                        (idx.parse::<usize>(), row.parse::<usize>(), col.parse::<usize>())
                    {
                        out.graphs.insert(i, (r, c, false));
                    }
                }
                ["R", row, start] => {
                    if let (Ok(r), Ok(s)) = (row.parse::<usize>(), start.parse::<usize>()) {
                        out.row_starts.insert(r, s);
                    }
                }
                [idx, row, col] => {
                    if let (Ok(i), Ok(r), Ok(c)) =
                        (idx.parse::<usize>(), row.parse::<usize>(), col.parse::<usize>())
                    {
                        out.graphs.insert(i, (r, c, false));
                    }
                }
                [idx, row] => {
                    if let (Ok(i), Ok(r)) = (idx.parse::<usize>(), row.parse::<usize>()) {
                        out.graphs.insert(i, (r, i, false));
                    }
                }
                _ => {}
            };
        }
    }
    Ok(out)
}

fn save_manifest(base_path: &str, manifest: &Manifest) -> Result<()> {
    let Some(path) = manifest_path(base_path) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut graph_entries: Vec<_> = manifest.graphs.iter().collect();
    graph_entries.sort_by_key(|(&idx, _)| idx);
    let mut row_entries: Vec<_> = manifest.row_starts.iter().collect();
    row_entries.sort_by_key(|(&row, _)| row);

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    for (row, start) in row_entries {
        writeln!(&mut file, "R {} {}", row, start)?;
    }
    for (idx, (row, col, complete)) in graph_entries {
        writeln!(&mut file, "G {} {} {} {}", idx, row, col, if *complete { 1 } else { 0 })?;
    }
    Ok(())
}

fn write_gallery_html(base_path: &str) -> Result<()> {
    let Some(path) = gallery_html_path(base_path) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let manifest = load_manifest(base_path)?;
    let (_, stem) = base_dir_and_stem(base_path);
    let mut out = String::new();
    out.push_str(r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link href="https://cdn.jsdelivr.net/npm/bootstrap@5.3.3/dist/css/bootstrap.min.css" rel="stylesheet">
  <style>
    table.graph-table { width: max-content; border-collapse: separate; border-spacing: 8px; }
    table.graph-table td { padding: 8px; border: 1px solid #dee2e6; background: #ffffff; vertical-align: top; }
    table.graph-table img { display: block; width: 100%; height: auto; }
  </style>
  <title>TraceForge Graphs</title>
</head>
<body>
  <div class="container-fluid py-3">
    <div class="mb-3 d-flex align-items-center gap-3">
      <label for="scaleRange" class="form-label mb-0">Scale</label>
      <input type="range" class="form-range w-auto" id="scaleRange" min="10" max="150" value="100" step="10" style="max-width: 240px;">
      <span id="scaleValue" class="small text-muted">100%</span>
    </div>
    <h1 class="h4 mb-3">Execution Graphs</h1>
    <div class="table-responsive">
      <table class="graph-table">
        <tbody>"#);

    let mut rows: BTreeMap<usize, Vec<(usize, usize, bool)>> = BTreeMap::new();
    for (&idx, &(row, col, complete)) in manifest.graphs.iter() {
        let row_start = *manifest.row_starts.get(&row).unwrap_or(&0);
        let abs_col = row_start + col;
        rows.entry(row).or_default().push((abs_col, idx, complete));
    }
    // Compute max cols per row to keep row-local padding only
    let mut row_max: BTreeMap<usize, usize> = BTreeMap::new();
    for (r, cols) in &rows {
        let mx = cols.iter().map(|(c, _, _)| *c).max().unwrap_or(0);
        row_max.insert(*r, mx);
    }

    for (row_idx, mut cells) in rows {
        cells.sort_by_key(|(col, _, _)| *col);
        out.push_str("<tr>");
        let mut current_col = 0usize;
        for (col, idx, complete) in cells {
            while current_col < col {
                out.push_str(r#"<td></td>"#);
                current_col += 1;
            }
            let img = format!("graphs/{}_g{:04}.png", stem, idx);
            out.push_str(&format!(
                r#"
          <td class="{complete_class}">
            <img src="{img}" alt="Graph {idx}">
          </td>"#,
                complete_class = if complete {
                    "border border-danger border-2 border-dashed"
                } else {
                    ""
                }
            ));
            current_col += 1;
        }
        let max_col = *row_max.get(&row_idx).unwrap_or(&current_col);
        while current_col <= max_col {
            out.push_str(r#"<td></td>"#);
            current_col += 1;
        }
        out.push_str("</tr>");
    }

    out.push_str(
        r#"
        </tbody>
      </table>
    </div>
    <script>
      (function() {
        const range = document.getElementById('scaleRange');
        const value = document.getElementById('scaleValue');
        const imgs = Array.from(document.querySelectorAll('table.graph-table img'));
        // Capture base widths once images have loaded
        imgs.forEach(img => {
          const setBase = () => {
            const base = img.naturalWidth || img.clientWidth || 240;
            img.dataset.baseWidth = base.toString();
            img.dataset.baseHeight = (img.naturalHeight || img.clientHeight || 180).toString();
          };
          if (img.complete) {
            setBase();
          } else {
            img.addEventListener('load', setBase, { once: true });
          }
        });
        function apply() {
          const scale = parseInt(range.value, 10) / 100;
          value.textContent = `${scale * 100}%`;
          imgs.forEach(img => {
            const baseW = parseFloat(img.dataset.baseWidth || '240');
            const baseH = parseFloat(img.dataset.baseHeight || '180');
            const w = baseW * scale;
            const h = baseH * scale;
            img.style.width = `${w}px`;
            img.style.height = `${h}px`;
            const td = img.closest('td');
            if (td) {
              td.style.width = `${w + 16}px`; // include padding/border slack
            }
          });
        }
        range.addEventListener('input', apply);
        apply();
      })();
    </script>
  </div>
</body>
</html>
"#,
    );

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(out.as_bytes())
}
fn base_dir_and_stem(base_path: &str) -> (PathBuf, String) {
    let mut p = PathBuf::from(base_path);
    if p.extension().is_some() {
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "graphs".to_string());
        let dir = p.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        (dir, stem)
    } else {
        if p.as_os_str().is_empty() {
            p = PathBuf::from(".");
        }
        let stem = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "graphs".to_string());
        (p, stem)
    }
}
