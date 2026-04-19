// To test IP filter:
//   sudo ... cargo run --bin backend -- --iface eth0 --filter-ip 185.125.190.57
// To test port filter:
//   sudo ... cargo run --bin backend -- --iface eth0 --filter-port 123
// To test both:
//   sudo ... cargo run --bin backend -- --iface eth0 --filter-ip 185.125.190.57 --filter-port 123
// Only connections matching the filter should appear in stdout and socket output.

use crate::output::ConnectionSnapshot;

pub fn apply_filters(
    snapshots: Vec<ConnectionSnapshot>,
    filter_ip: Option<&str>,
    filter_port: Option<u16>,
) -> Vec<ConnectionSnapshot> {
    if filter_ip.is_none() && filter_port.is_none() {
        return snapshots;
    }

    snapshots
        .into_iter()
        .filter(|conn| {
            let ip_match = match filter_ip {
                Some(ip) => conn.src_ip == ip || conn.dst_ip == ip,
                None => true,
            };

            let port_match = match filter_port {
                Some(port) => conn.src_port == port || conn.dst_port == port,
                None => true,
            };

            ip_match && port_match
        })
        .collect()
}
