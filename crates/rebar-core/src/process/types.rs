use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessId {
    node_id: u64,
    local_id: u64,
}

impl ProcessId {
    pub fn new(node_id: u64, local_id: u64) -> Self {
        Self { node_id, local_id }
    }

    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    pub fn local_id(&self) -> u64 {
        self.local_id
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}.{}>", self.node_id, self.local_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn process_id_equality() {
        let a = ProcessId::new(1, 42);
        let b = ProcessId::new(1, 42);
        assert_eq!(a, b);
    }

    #[test]
    fn process_id_inequality_different_node() {
        assert_ne!(ProcessId::new(1, 42), ProcessId::new(2, 42));
    }

    #[test]
    fn process_id_inequality_different_local() {
        assert_ne!(ProcessId::new(1, 42), ProcessId::new(1, 43));
    }

    #[test]
    fn process_id_is_copy() {
        let a = ProcessId::new(1, 42);
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn process_id_display() {
        assert_eq!(format!("{}", ProcessId::new(1, 42)), "<1.42>");
    }

    #[test]
    fn process_id_hash_dedup() {
        let mut set = HashSet::new();
        set.insert(ProcessId::new(1, 1));
        set.insert(ProcessId::new(1, 1));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn process_id_accessors() {
        let pid = ProcessId::new(5, 99);
        assert_eq!(pid.node_id(), 5);
        assert_eq!(pid.local_id(), 99);
    }

    #[test]
    fn process_id_debug() {
        let pid = ProcessId::new(1, 1);
        let debug = format!("{:?}", pid);
        assert!(debug.contains("ProcessId"));
    }
}
