use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let header = Row::new(vec![
        Cell::from("PID").style(Style::default().fg(Color::Blue).bold()),
        Cell::from("Name").style(Style::default().fg(Color::Blue).bold()),
        Cell::from("Parent").style(Style::default().fg(Color::Blue).bold()),
        Cell::from("Uptime").style(Style::default().fg(Color::Blue).bold()),
        Cell::from("Type").style(Style::default().fg(Color::Blue).bold()),
    ]);

    let rows: Vec<Row> = app
        .processes
        .iter()
        .skip(app.scroll_offset)
        .map(|p| {
            let pid = format!("<{}.{}>", p.pid.node_id(), p.pid.local_id());
            let name = p.name.clone().unwrap_or_else(|| "-".into());
            let parent = p
                .parent
                .map(|pp| format!("<{}.{}>", pp.node_id(), pp.local_id()))
                .unwrap_or_else(|| "-".into());
            let uptime = format_uptime(p.uptime_ms);
            let typ = if p.is_supervisor {
                "supervisor"
            } else {
                "process"
            };

            Row::new(vec![
                Cell::from(pid),
                Cell::from(name),
                Cell::from(parent),
                Cell::from(uptime),
                Cell::from(typ),
            ])
        })
        .collect();

    let sort_hint = format!(" Process List (sort: {}, press 's' to cycle) ", app.sort_column.label());

    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Min(16),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(sort_hint)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    );

    frame.render_widget(table, area);
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
