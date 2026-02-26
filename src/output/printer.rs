
/// Print a host-prefixed line with color.
pub fn print_host_line(host: &str, status: &str, detail: &str) {
    let max_name_len = 12;
    let padded = format!("{:width$}", host, width = max_name_len);
    let (symbol, color_code) = match status {
        "ok" => ("✓", "\x1b[32m"),      // green
        "error" => ("✗", "\x1b[31m"),   // red
        "skip" => ("⊘", "\x1b[33m"),    // yellow
        _ => ("·", "\x1b[37m"),          // white
    };

    println!(
        "[{padded}]  {color}{symbol}\x1b[0m {detail}",
        padded = padded,
        color = color_code,
        symbol = symbol,
        detail = detail
    );
}
