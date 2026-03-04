pub mod events;
pub mod list;
pub mod tree;

pub fn format_uptime(ms: u64) -> String {
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
