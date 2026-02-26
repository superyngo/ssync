/// Return the CMD probe command for a given metric.
/// CMD support is minimal / nice-to-have.
pub fn command_for(metric: &str) -> String {
    match metric {
        "online" => "echo ok".to_string(),
        "system_info" => "systeminfo".to_string(),
        "cpu_arch" => "wmic cpu get AddressWidth /value".to_string(),
        "memory" => "wmic OS get FreePhysicalMemory,TotalVisibleMemorySize /value".to_string(),
        "swap" => "wmic pagefile get AllocatedBaseSize,CurrentUsage /value".to_string(),
        "disk" => "wmic logicaldisk get size,freespace,caption /value".to_string(),
        "cpu_load" => "wmic cpu get LoadPercentage /value".to_string(),
        "network" => "ipconfig".to_string(),
        "battery" => "wmic path Win32_Battery get EstimatedChargeRemaining /value".to_string(),
        _ => String::new(),
    }
}
