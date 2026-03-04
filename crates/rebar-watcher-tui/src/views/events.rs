use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem};

use rebar_core::events::LifecycleEvent;

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .events
        .iter()
        .rev()
        .skip(app.scroll_offset)
        .map(|event| {
            let (label, style) = event_style(event);
            let detail = event_detail(event);
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("[{}] ", label),
                    style,
                ),
                Span::raw(detail),
            ]))
        })
        .collect();

    let block = Block::default()
        .title(format!(" Event Log ({} events) ", app.events.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue));

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn event_style(event: &LifecycleEvent) -> (&str, Style) {
    match event {
        LifecycleEvent::ProcessSpawned { .. } | LifecycleEvent::ChildStarted { .. } => {
            ("SPAWN", Style::default().fg(Color::Green))
        }
        LifecycleEvent::ProcessExited { .. }
        | LifecycleEvent::ChildExited { .. }
        | LifecycleEvent::SupervisorMaxRestartsExceeded { .. } => {
            ("EXIT", Style::default().fg(Color::Red))
        }
        LifecycleEvent::ChildRestarted { .. } => {
            ("RESTART", Style::default().fg(Color::Yellow))
        }
        LifecycleEvent::SupervisorStarted { .. } => {
            ("SUPER", Style::default().fg(Color::Magenta))
        }
    }
}

fn event_detail(event: &LifecycleEvent) -> String {
    match event {
        LifecycleEvent::ProcessSpawned { pid, name, .. } => {
            format!("Process {} spawned{}", pid, name.as_deref().map(|n| format!(" ({})", n)).unwrap_or_default())
        }
        LifecycleEvent::ProcessExited { pid, reason, .. } => {
            format!("Process {} exited: {:?}", pid, reason)
        }
        LifecycleEvent::SupervisorStarted { pid, strategy, child_ids, .. } => {
            format!("Supervisor {} started ({:?}) children=[{}]", pid, strategy, child_ids.join(", "))
        }
        LifecycleEvent::ChildStarted { supervisor_pid, child_pid, child_id, .. } => {
            format!("Child {} ({}) started under {}", child_id, child_pid, supervisor_pid)
        }
        LifecycleEvent::ChildExited { child_id, child_pid, reason, .. } => {
            format!("Child {} ({}) exited: {:?}", child_id, child_pid, reason)
        }
        LifecycleEvent::ChildRestarted { child_id, old_pid, new_pid, restart_count, .. } => {
            format!("Child {} restarted {} -> {} (#{})", child_id, old_pid, new_pid, restart_count)
        }
        LifecycleEvent::SupervisorMaxRestartsExceeded { pid, .. } => {
            format!("Supervisor {} max restarts exceeded", pid)
        }
    }
}
