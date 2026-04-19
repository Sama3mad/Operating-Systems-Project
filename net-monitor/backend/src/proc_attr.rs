use std::collections::HashMap;
use std::fs;
use std::net::IpAddr;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use crate::parser::Protocol;

#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub username: String,
}

static PASSWD_CACHE: OnceLock<HashMap<u32, String>> = OnceLock::new();

fn passwd_map() -> &'static HashMap<u32, String> {
    PASSWD_CACHE.get_or_init(|| {
        let mut map = HashMap::new();
        if let Ok(contents) = fs::read_to_string("/etc/passwd") {
            for line in contents.lines() {
                let fields: Vec<&str> = line.split(':').collect();
                if fields.len() > 2 && let Ok(uid) = fields[2].parse::<u32>() {
                    map.insert(uid, fields[0].to_string());
                }
            }
        }
        map
    })
}

pub fn attribute(
    _src_ip: IpAddr,
    src_port: u16,
    _dst_ip: IpAddr,
    dst_port: u16,
    protocol: Protocol,
) -> Option<ProcessInfo> {
    let local_port = src_port;
    let inode = find_inode(src_port, dst_port, protocol);

    if let Some(inode_value) = inode {
        if inode_value != "0" && let Some(pid) = find_pid_for_inode(&inode_value) {
            let (name, uid) = read_process_name_uid(pid)?;
            let username = passwd_map().get(&uid).cloned().unwrap_or_else(|| uid.to_string());
            return Some(ProcessInfo {
                pid,
                name,
                username,
            });
        }
    }

    if matches!(protocol, Protocol::Tcp)
        && let Some((pid, process_name)) = ss_fallback(local_port)
    {
        let uid = read_process_name_uid(pid).map(|(_, uid)| uid)?;
        let username = passwd_map().get(&uid).cloned().unwrap_or_else(|| uid.to_string());
        return Some(ProcessInfo {
            pid,
            name: process_name,
            username,
        });
    }

    None
}

fn find_inode(src_port: u16, dst_port: u16, protocol: Protocol) -> Option<String> {
    let local_ports = [src_port, dst_port];
    let files = match protocol {
        Protocol::Tcp => ["/proc/net/tcp", "/proc/net/tcp6"],
        Protocol::Udp => ["/proc/net/udp", "/proc/net/udp6"],
    };

    for file in files {
        if let Ok(contents) = fs::read_to_string(file) {
            for line in contents.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() < 10 {
                    continue;
                }

                let local = cols[1];
                let mut parts = local.split(':');
                let _ip_hex = parts.next();
                let port_hex = parts.next();
                let Some(port_hex) = port_hex else {
                    continue;
                };

                let Ok(port) = u16::from_str_radix(port_hex, 16) else {
                    continue;
                };

                if local_ports.contains(&port) {
                    return Some(cols[9].to_string());
                }
            }
        }
    }

    None
}

fn find_pid_for_inode(inode: &str) -> Option<u32> {
    let proc_dir = fs::read_dir("/proc").ok()?;
    for entry in proc_dir.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let Ok(pid) = file_name.parse::<u32>() else {
            continue;
        };

        let fd_dir = entry.path().join("fd");
        let fd_entries = match fs::read_dir(fd_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for fd_entry in fd_entries.flatten() {
            let target = match fs::read_link(fd_entry.path()) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if socket_target_matches_inode(&target, inode) {
                return Some(pid);
            }
        }
    }

    None
}

fn socket_target_matches_inode(target: &Path, inode: &str) -> bool {
    let target = target.to_string_lossy();
    target == format!("socket:[{inode}]")
}

fn read_process_name_uid(pid: u32) -> Option<(String, u32)> {
    let status_path = format!("/proc/{pid}/status");
    let content = fs::read_to_string(status_path).ok()?;
    let mut name: Option<String> = None;
    let mut uid: Option<u32> = None;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("Name:") {
            name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Uid:") {
            let first = rest.split_whitespace().next();
            if let Some(first) = first && let Ok(parsed) = first.parse::<u32>() {
                uid = Some(parsed);
            }
        }
    }

    Some((name?, uid?))
}

fn ss_fallback(local_port: u16) -> Option<(u32, String)> {
    let output = Command::new("ss")
        .args(["-tnp", "src", &format!(":{local_port}")])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if !line.contains("users:(") {
            continue;
        }
        if let Some(result) = parse_ss_users_field(line) {
            return Some(result);
        }
    }

    None
}

fn parse_ss_users_field(line: &str) -> Option<(u32, String)> {
    let users_idx = line.find("users:(")?;
    let users = &line[users_idx..];

    let pid_idx = users.find("pid=")?;
    let pid_part = &users[pid_idx + 4..];
    let pid_str: String = pid_part
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if pid_str.is_empty() {
        return None;
    }
    let pid = pid_str.parse::<u32>().ok()?;

    let first_quote = users.find('"')?;
    let rest = &users[first_quote + 1..];
    let second_quote = rest.find('"')?;
    let name = rest[..second_quote].to_string();
    if name.is_empty() {
        return None;
    }

    Some((pid, name))
}
