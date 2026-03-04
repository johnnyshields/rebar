use std::collections::HashMap;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    // Build parent -> children map
    let mut children_map: HashMap<String, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();

    for (i, proc) in app.processes.iter().enumerate() {
        if let Some(parent) = proc.parent {
            let key = format!("{}.{}", parent.node_id(), parent.local_id());
            children_map.entry(key).or_default().push(i);
        } else {
            roots.push(i);
        }
    }

    // If no parent info, show all as roots
    if roots.is_empty() && !app.processes.is_empty() {
        roots = (0..app.processes.len()).collect();
    }

    let mut items = Vec::new();
    for &idx in &roots {
        build_tree_items(&app.processes, &children_map, idx, 0, &mut items);
    }

    let visible: Vec<ListItem> = items
        .into_iter()
        .skip(app.scroll_offset)
        .map(|(line, depth)| {
            let indent = "  ".repeat(depth);
            ListItem::new(Line::raw(format!("{}{}", indent, line)))
        })
        .collect();

    let block = Block::default()
        .title(" Supervision Tree ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue));

    let list = List::new(visible).block(block);
    frame.render_widget(list, area);
}

fn build_tree_items(
    processes: &[rebar_core::process::table::ProcessInfo],
    children_map: &HashMap<String, Vec<usize>>,
    idx: usize,
    depth: usize,
    items: &mut Vec<(String, usize)>,
) {
    let proc = &processes[idx];
    let icon = if proc.is_supervisor { "\u{25BC}" } else { "\u{25CF}" };
    let name = proc.name.as_deref().unwrap_or("process");
    let pid = format!("<{}.{}>", proc.pid.node_id(), proc.pid.local_id());
    let uptime = format_uptime(proc.uptime_ms);

    items.push((format!("{} {} {} [{}]", icon, name, pid, uptime), depth));

    let key = format!("{}.{}", proc.pid.node_id(), proc.pid.local_id());
    if let Some(kids) = children_map.get(&key) {
        for &kid_idx in kids {
            build_tree_items(processes, children_map, kid_idx, depth + 1, items);
        }
    }
}

fn format_uptime(ms: u64) -> String {
    if ms < 1000 {
        return format!("{}ms", ms);
    }
    let s = ms / 1000;
    if s < 60 {
        return format!("{}s", s);
    }
    let m = s / 60;
    if m < 60 {
        return format!("{}m {}s", m, s % 60);
    }
    let h = m / 60;
    format!("{}h {}m", h, m % 60)
}
