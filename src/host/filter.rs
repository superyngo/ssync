use crate::config::schema::{HostEntry, ShellType};

/// Filter hosts based on CLI parameters.
/// Matches groups from host[].groups tags.
#[allow(dead_code)]
pub fn filter_hosts<'a>(
    hosts: &'a [HostEntry],
    groups: &[String],
    host_names: &[String],
    all: bool,
    shells: &[ShellType],
) -> Vec<&'a HostEntry> {
    if all {
        return hosts.iter().collect();
    }

    let mut result: Vec<&HostEntry> = hosts.iter().collect();

    if !groups.is_empty() {
        result.retain(|h| h.groups.iter().any(|g| groups.contains(g)));
    }

    if !host_names.is_empty() {
        result.retain(|h| host_names.contains(&h.name));
    }

    if !shells.is_empty() {
        result.retain(|h| shells.contains(&h.shell));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::ShellType;

    fn make_hosts() -> Vec<HostEntry> {
        vec![
            HostEntry {
                name: "a".into(),
                ssh_host: "a".into(),
                shell: ShellType::Sh,
                groups: vec!["web".into()],
                proxy_jump: None,
            },
            HostEntry {
                name: "b".into(),
                ssh_host: "b".into(),
                shell: ShellType::PowerShell,
                groups: vec!["db".into()],
                proxy_jump: None,
            },
            HostEntry {
                name: "c".into(),
                ssh_host: "c".into(),
                shell: ShellType::Sh,
                groups: vec!["web".into()],
                proxy_jump: None,
            },
        ]
    }

    #[test]
    fn test_filter_all() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &[], &[], true, &[]);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_filter_by_group() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &["web".into()], &[], false, &[]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "a");
        assert_eq!(result[1].name, "c");
    }

    #[test]
    fn test_filter_by_host_name() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &[], &["b".into()], false, &[]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "b");
    }

    #[test]
    fn test_filter_intersection() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &["web".into()], &["c".into()], false, &[]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "c");
    }

    #[test]
    fn test_filter_by_shell() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &[], &[], false, &[ShellType::Sh]);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|h| h.shell == ShellType::Sh));
    }

    #[test]
    fn test_filter_by_shell_powershell() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &[], &[], false, &[ShellType::PowerShell]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "b");
    }

    #[test]
    fn test_filter_by_shell_multiple() {
        let hosts = make_hosts();
        let result = filter_hosts(
            &hosts,
            &[],
            &[],
            false,
            &[ShellType::Sh, ShellType::PowerShell],
        );
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_filter_no_shell_filter_returns_all_when_no_other_filter() {
        let hosts = make_hosts();
        let result = filter_hosts(&hosts, &[], &[], false, &[]);
        assert_eq!(result.len(), 3);
    }
}
