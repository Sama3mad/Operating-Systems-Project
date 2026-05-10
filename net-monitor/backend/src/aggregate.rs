use std::collections::HashMap;

use serde::Serialize;

use crate::output::ConnectionSnapshot;

#[derive(Serialize, Clone, Debug)]
pub struct ProcessRollup {
    pub process_label: String,
    pub username: String,
    pub bytes_in_per_sec: u64,
    pub bytes_out_per_sec: u64,
    pub flow_count: u32,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProtocolRollup {
    pub protocol: String,
    pub bytes_in_per_sec: u64,
    pub bytes_out_per_sec: u64,
    pub flow_count: u32,
}

#[derive(Serialize, Clone, Debug)]
pub struct AggregatePayload {
    pub timestamp_unix: i64,
    pub by_process: Vec<ProcessRollup>,
    pub by_protocol: Vec<ProtocolRollup>,
}

/// Roll up the filtered per-connection snapshot for the same 1s window.
pub fn compute_aggregate(filtered: &[ConnectionSnapshot], timestamp_unix: i64) -> AggregatePayload {
    let mut process_map: HashMap<(String, String), (u64, u64, u32)> = HashMap::new();
    let mut protocol_map: HashMap<String, (u64, u64, u32)> = HashMap::new();

    for row in filtered {
        let pname = row
            .process_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("<unattributed>");
        let user = row
            .username
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("—");
        let pk = (pname.to_string(), user.to_string());
        let e = process_map.entry(pk).or_insert((0, 0, 0));
        e.0 = e.0.saturating_add(row.bytes_in_per_sec);
        e.1 = e.1.saturating_add(row.bytes_out_per_sec);
        e.2 = e.2.saturating_add(1);

        let proto = if row.protocol.is_empty() {
            "?"
        } else {
            row.protocol.as_str()
        };
        let pe = protocol_map
            .entry(proto.to_string())
            .or_insert((0, 0, 0));
        pe.0 = pe.0.saturating_add(row.bytes_in_per_sec);
        pe.1 = pe.1.saturating_add(row.bytes_out_per_sec);
        pe.2 = pe.2.saturating_add(1);
    }

    let mut by_process: Vec<ProcessRollup> = process_map
        .into_iter()
        .map(|((process_label, username), (bi, bo, fc))| ProcessRollup {
            process_label,
            username,
            bytes_in_per_sec: bi,
            bytes_out_per_sec: bo,
            flow_count: fc,
        })
        .collect();
    by_process.sort_by(|a, b| {
        let ta = a.bytes_in_per_sec.saturating_add(a.bytes_out_per_sec);
        let tb = b.bytes_in_per_sec.saturating_add(b.bytes_out_per_sec);
        tb.cmp(&ta)
    });

    let mut by_protocol: Vec<ProtocolRollup> = protocol_map
        .into_iter()
        .map(|(protocol, (bi, bo, fc))| ProtocolRollup {
            protocol,
            bytes_in_per_sec: bi,
            bytes_out_per_sec: bo,
            flow_count: fc,
        })
        .collect();
    by_protocol.sort_by(|a, b| {
        let ta = a.bytes_in_per_sec.saturating_add(a.bytes_out_per_sec);
        let tb = b.bytes_in_per_sec.saturating_add(b.bytes_out_per_sec);
        tb.cmp(&ta)
    });

    AggregatePayload {
        timestamp_unix,
        by_process,
        by_protocol,
    }
}
