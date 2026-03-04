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

#[cfg(test)]
mod tests {
    use super::*;
    use rebar_core::events::LifecycleEvent;
    use rebar_core::process::ProcessId;

    fn make_event(local_id: u64) -> LifecycleEvent {
        LifecycleEvent::ProcessSpawned {
            pid: ProcessId::new(0, local_id),
            parent: None,
            name: None,
            timestamp: 0,
        }
    }

    fn make_process(local_id: u64, name: Option<&str>, uptime_ms: u64, is_supervisor: bool) -> ProcessInfo {
        ProcessInfo {
            pid: ProcessId::new(0, local_id),
            name: name.map(|s| s.to_string()),
            parent: None,
            uptime_ms,
            is_supervisor,
        }
    }

    #[test]
    fn push_event_capacity_limit() {
        let mut app = App::new();
        for i in 0..1005 {
            app.push_event(make_event(i));
        }
        assert_eq!(app.events.len(), 1000);
        // First 5 should have been evicted; first remaining should be local_id=5
        match &app.events[0] {
            LifecycleEvent::ProcessSpawned { pid, .. } => assert_eq!(pid.local_id(), 5),
            _ => panic!("unexpected event type"),
        }
    }

    #[test]
    fn sort_processes_by_pid() {
        let mut app = App::new();
        app.processes = vec![make_process(3, None, 0, false), make_process(1, None, 0, false)];
        app.sort_column = SortColumn::Pid;
        app.sort_processes();
        assert_eq!(app.processes[0].pid.local_id(), 1);
        assert_eq!(app.processes[1].pid.local_id(), 3);
    }

    #[test]
    fn sort_processes_by_name() {
        let mut app = App::new();
        app.processes = vec![
            make_process(1, Some("zebra"), 0, false),
            make_process(2, Some("alpha"), 0, false),
        ];
        app.sort_column = SortColumn::Name;
        app.sort_processes();
        assert_eq!(app.processes[0].name.as_deref(), Some("alpha"));
        assert_eq!(app.processes[1].name.as_deref(), Some("zebra"));
    }

    #[test]
    fn sort_processes_by_uptime() {
        let mut app = App::new();
        app.processes = vec![make_process(1, None, 500, false), make_process(2, None, 100, false)];
        app.sort_column = SortColumn::Uptime;
        app.sort_processes();
        assert_eq!(app.processes[0].uptime_ms, 100);
        assert_eq!(app.processes[1].uptime_ms, 500);
    }

    #[test]
    fn sort_processes_by_type() {
        let mut app = App::new();
        app.processes = vec![make_process(1, None, 0, false), make_process(2, None, 0, true)];
        app.sort_column = SortColumn::Type;
        app.sort_processes();
        // Supervisors sort first (is_supervisor=true => !true=false => 0)
        assert!(app.processes[0].is_supervisor);
        assert!(!app.processes[1].is_supervisor);
    }

    #[test]
    fn scroll_up_saturates_at_zero() {
        let mut app = App::new();
        app.scroll_offset = 0;
        app.scroll_up();
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_respects_max() {
        let mut app = App::new();
        app.scroll_down(3);
        assert_eq!(app.scroll_offset, 1);
        app.scroll_down(3);
        assert_eq!(app.scroll_offset, 2);
        app.scroll_down(3);
        assert_eq!(app.scroll_offset, 2); // capped at max-1
    }
}
