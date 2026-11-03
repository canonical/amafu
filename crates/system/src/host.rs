use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCapacity {
    pub cpu_count: u32,
    pub memory_mebibytes: u64,
}

fn total_memory_kibibytes(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

#[must_use]
pub fn capacity(proc: &Path) -> HostCapacity {
    let cpu_count = std::thread::available_parallelism()
        .ok()
        .and_then(|count| u32::try_from(count.get()).ok())
        .unwrap_or(1);
    let memory_mebibytes = std::fs::read_to_string(proc.join("meminfo"))
        .ok()
        .and_then(|meminfo| total_memory_kibibytes(&meminfo))
        .map_or(0, |kibibytes| kibibytes / 1024);
    HostCapacity { cpu_count, memory_mebibytes }
}

#[must_use]
pub fn cpu_flags(cpuinfo: &str) -> Vec<String> {
    cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("flags").and_then(|rest| rest.trim_start().strip_prefix(':')))
        .map(|flags| flags.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

#[must_use]
pub fn host_cpu_flags(proc: &Path) -> Vec<String> {
    std::fs::read_to_string(proc.join("cpuinfo")).map(|cpuinfo| cpu_flags(&cpuinfo)).unwrap_or_default()
}

#[must_use]
pub fn uptime_seconds(proc: &Path) -> Option<f64> {
    std::fs::read_to_string(proc.join("uptime")).ok()?.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_memory_is_parsed_from_meminfo() {
        let meminfo = "MemTotal:       16384000 kB\nMemFree:         1000 kB\n";
        assert_eq!(total_memory_kibibytes(meminfo), Some(16_384_000));
        assert_eq!(total_memory_kibibytes("garbage"), None);
    }

    #[test]
    fn cpu_flags_are_read_from_the_first_processor() {
        let cpuinfo = "processor\t: 0\nflags\t\t: fpu sse2 avx2\n\nprocessor\t: 1\nflags\t\t: fpu\n";
        assert_eq!(cpu_flags(cpuinfo), vec!["fpu", "sse2", "avx2"]);
        assert_eq!(cpu_flags(""), Vec::<String>::new());
    }
}
