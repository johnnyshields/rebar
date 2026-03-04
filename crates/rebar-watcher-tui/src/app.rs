use std::collections::VecDeque;

use rebar_core::events::LifecycleEvent;
use rebar_core::process::table::ProcessInfo;

pub enum View {
    Tree,
    List,
    Events,
}

pub enum SortColumn {
    Pid,
    Name,
    Parent,
    Uptime,
    Type,
}

impl SortColumn {
    pub fn next(&self) -> Self {
        match self {
            SortColumn::Pid => SortColumn::Name,
            SortColumn::Name => SortColumn::Parent,
            SortColumn::Parent => SortColumn::Uptime,
            SortColumn::Uptime => SortColumn::Type,
            SortColumn::Type => SortColumn::Pid,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            SortColumn::Pid => "PID",
            SortColumn::Name => "Name",
            SortColumn::Parent => "Parent",
            SortColumn::Uptime => "Uptime",
            SortColumn::Type => "Type",
        }
    }
}

pub struct App {
    pub view: View,
    pub processes: Vec<ProcessInfo>,
    pub events: VecDeque<LifecycleEvent>,
    pub scroll_offset: usize,
    pub sort_column: SortColumn,
    pub should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            view: View::Tree,
            processes: Vec::new(),
            events: VecDeque::with_capacity(1000),
            scroll_offset: 0,
            sort_column: SortColumn::Pid,
            should_quit: false,
        }
    }

    pub fn push_event(&mut self, event: LifecycleEvent) {
        if self.events.len() >= 1000 {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    pub fn sort_processes(&mut self) {
        match self.sort_column {
            SortColumn::Pid => self.processes.sort_by_key(|p| p.pid.local_id()),
            SortColumn::Name => self.processes.sort_by(|a, b| {
                a.name.as_deref().unwrap_or("").cmp(b.name.as_deref().unwrap_or(""))
            }),
            SortColumn::Parent => self.processes.sort_by_key(|p| {
                p.parent.map(|pp| pp.local_id()).unwrap_or(0)
            }),
            SortColumn::Uptime => self.processes.sort_by_key(|p| p.uptime_ms),
            SortColumn::Type => self.processes.sort_by_key(|p| !p.is_supervisor),
        }
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self, max: usize) {
        if self.scroll_offset < max.saturating_sub(1) {
            self.scroll_offset += 1;
        }
    }
}
