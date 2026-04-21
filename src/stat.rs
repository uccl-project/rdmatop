use crate::netlink::*;
use crate::rdma::*;
use std::io;

#[derive(Clone, Debug)]
/// A single hardware counter name/value pair.
pub struct HwCounter {
    pub name: String,
    pub value: u64,
}

#[derive(Clone, Debug)]
/// Per-port statistics snapshot with all hw counters.
pub struct PortStat {
    pub dev_name: String,
    pub port: u32,
    pub counters: Vec<HwCounter>,
}

impl PortStat {
    /// Look up a counter value by name.
    pub fn counter_value(&self, name: &str) -> Option<u64> {
        self.counters
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.value)
    }
}

struct RdmaDev {
    idx: u32,
    name: String,
    num_ports: u32,
}

fn parse_dev(nlmsg: &NlMsg) -> Option<RdmaDev> {
    let mut dev = RdmaDev {
        idx: 0,
        name: String::new(),
        num_ports: 1,
    };
    for nla in nlmsg.attrs() {
        match nla.attr_type {
            RDMA_NLDEV_ATTR_DEV_INDEX => dev.idx = nla.u32(),
            RDMA_NLDEV_ATTR_DEV_NAME => dev.name = nla.str().to_string(),
            RDMA_NLDEV_ATTR_PORT_INDEX => dev.num_ports = nla.u32(),
            _ => {}
        }
    }
    (!dev.name.is_empty()).then_some(dev)
}

fn parse_hw_counter(entry: &Nla) -> HwCounter {
    let mut name = String::new();
    let mut value = 0u64;
    for f in entry.nested() {
        match f.attr_type {
            RDMA_NLDEV_ATTR_STAT_HWCOUNTER_ENTRY_NAME => name = f.str().to_string(),
            RDMA_NLDEV_ATTR_STAT_HWCOUNTER_ENTRY_VALUE => value = f.u64(),
            _ => {}
        }
    }
    HwCounter { name, value }
}

fn parse_port_stat(nlmsg: &NlMsg) -> Option<PortStat> {
    let mut stat = PortStat {
        dev_name: String::new(),
        port: 0,
        counters: Vec::new(),
    };
    for nla in nlmsg.attrs() {
        match nla.attr_type {
            RDMA_NLDEV_ATTR_DEV_NAME => stat.dev_name = nla.str().to_string(),
            RDMA_NLDEV_ATTR_PORT_INDEX => stat.port = nla.u32(),
            RDMA_NLDEV_ATTR_STAT_HWCOUNTERS => {
                stat.counters = nla
                    .nested()
                    .filter(|e| e.attr_type == RDMA_NLDEV_ATTR_STAT_HWCOUNTER_ENTRY)
                    .map(|e| parse_hw_counter(&e))
                    .collect();
            }
            _ => {}
        }
    }
    (!stat.dev_name.is_empty()).then_some(stat)
}

fn enumerate_devices(sock: &NlSocket) -> io::Result<Vec<RdmaDev>> {
    let msg = NlMsgBuilder::new(
        rdma_nl_get_type(RDMA_NL_NLDEV, RDMA_NLDEV_CMD_GET),
        NLM_F_REQUEST | NLM_F_ACK | NLM_F_DUMP,
        1,
    )
    .build();
    collect_responses(sock, msg, parse_dev)
}

fn query_port_stats(
    sock: &NlSocket,
    dev_idx: u32,
    port: u32,
    seq: u32,
) -> io::Result<Vec<PortStat>> {
    let msg = NlMsgBuilder::new(
        rdma_nl_get_type(RDMA_NL_NLDEV, RDMA_NLDEV_CMD_STAT_GET),
        NLM_F_REQUEST | NLM_F_ACK,
        seq,
    )
    .put_u32(RDMA_NLDEV_ATTR_DEV_INDEX, dev_idx)
    .put_u32(RDMA_NLDEV_ATTR_PORT_INDEX, port)
    .build();
    collect_responses(sock, msg, parse_port_stat)
}

/// Query all RDMA device/port hw counters via netlink.
/// Query all RDMA device/port hw counters via netlink.
pub fn read_all_stats() -> io::Result<Vec<PortStat>> {
    let sock = NlSocket::open(NETLINK_RDMA)?;
    let devs = enumerate_devices(&sock)?;

    let sock = NlSocket::open(NETLINK_RDMA)?;
    let mut all = Vec::new();
    let mut seq = 100u32;
    for dev in &devs {
        for port in 1..=dev.num_ports {
            all.extend(query_port_stats(&sock, dev.idx, port, seq)?);
            seq += 1;
        }
    }
    Ok(all)
}

#[derive(Clone, Debug)]
/// Queue pair info with device, PID, and process name.
pub struct QpInfo {
    pub dev_name: String,
    pub lqpn: u32,
    pub qp_type: u8,
    pub state: u8,
    pub pid: u32,
    pub comm: String,
}

// This matches iproute2's res_qp_idx_parse_cb which reads from tb[] directly.

fn parse_qp_response(nlmsg: &NlMsg, dev_name: &str) -> Vec<QpInfo> {
    let mut name = dev_name.to_string();
    let mut qps = Vec::new();
    for nla in nlmsg.attrs() {
        match nla.attr_type {
            RDMA_NLDEV_ATTR_DEV_NAME => name = nla.str().to_string(),
            RDMA_NLDEV_ATTR_RES_QP => {
                for entry in nla.nested() {
                    if entry.attr_type == RDMA_NLDEV_ATTR_RES_QP_ENTRY {
                        if let Some(qp) = parse_single_qp(&entry, &name) {
                            qps.push(qp);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    qps
}

fn parse_single_qp(nla: &Nla, dev_name: &str) -> Option<QpInfo> {
    let mut qp = QpInfo {
        dev_name: dev_name.to_string(),
        lqpn: 0,
        qp_type: 0,
        state: 0,
        pid: 0,
        comm: String::new(),
    };
    for attr in nla.nested() {
        match attr.attr_type {
            RDMA_NLDEV_ATTR_RES_LQPN => qp.lqpn = attr.u32(),
            RDMA_NLDEV_ATTR_RES_TYPE if !attr.data.is_empty() => {
                qp.qp_type = attr.data[0];
            }
            RDMA_NLDEV_ATTR_RES_STATE if !attr.data.is_empty() => {
                qp.state = attr.data[0];
            }
            RDMA_NLDEV_ATTR_RES_PID => {
                qp.pid = attr.u32();
                qp.comm = read_proc_comm(qp.pid);
            }
            RDMA_NLDEV_ATTR_RES_KERN_NAME => qp.comm = attr.str().to_string(),
            _ => {}
        }
    }
    (qp.pid > 0).then_some(qp)
}

fn read_proc_comm(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Query all RDMA QPs via netlink (like `rdma resource show qp`).
/// Query all RDMA QPs via netlink, returning PID and device mappings.
pub fn read_all_qps() -> io::Result<Vec<QpInfo>> {
    let sock = NlSocket::open(NETLINK_RDMA)?;
    let devs = enumerate_devices(&sock)?;
    drop(sock);

    let mut all = Vec::new();
    let mut seq = 200u32;
    for dev in &devs {
        let sock = NlSocket::open(NETLINK_RDMA)?;
        let msg = NlMsgBuilder::new(
            rdma_nl_get_type(RDMA_NL_NLDEV, RDMA_NLDEV_CMD_RES_QP_GET),
            NLM_F_REQUEST | NLM_F_ACK | NLM_F_DUMP,
            seq,
        )
        .put_u32(RDMA_NLDEV_ATTR_DEV_INDEX, dev.idx)
        .build();

        if let Ok(bufs) = sock.request(msg) {
            for buf in bufs {
                for nlmsg in NlMsgIter::new(&buf) {
                    if nlmsg.is_done() || nlmsg.is_error() {
                        continue;
                    }
                    all.extend(parse_qp_response(&nlmsg, &dev.name));
                }
            }
        }
        seq += 1;
    }
    Ok(all)
}

/// Aggregate QPs by (pid, comm, dev_name), enriched with /proc data.
#[derive(Clone, Debug)]
/// Process info enriched with /proc data for htop-style display.
pub struct ProcessRdmaInfo {
    pub pid: u32,
    pub dev_name: String,
    pub qp_count: usize,
    pub user: String,
    pub nice: i32,
    pub state: char,
    pub virt_kb: u64,
    pub res_kb: u64,
    pub shr_kb: u64,
    pub mem_pct: f32,
    pub threads: u32,
    pub cmdline: String,
}

/// Raw fields from /proc/<pid>/stat
struct ProcStat {
    state: char,
    nice: i32,
    vsize: u64,
    rss_pages: u64,
    num_threads: u32,
}

fn read_proc_stat(pid: u32) -> Option<ProcStat> {
    let data = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    // Fields after last ')' to avoid comm with spaces
    let rest = data.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // fields[0]=state, [1]=ppid, ..., [11]=utime, [12]=stime, ..., [16]=nice, ..., [17]=num_threads, ..., [20]=vsize, [21]=rss
    if fields.len() < 22 {
        return None;
    }
    Some(ProcStat {
        state: fields[0].chars().next().unwrap_or('?'),
        nice: fields[16].parse().unwrap_or(0),
        num_threads: fields[17].parse().unwrap_or(1),
        vsize: fields[20].parse().unwrap_or(0),
        rss_pages: fields[21].parse().unwrap_or(0),
    })
}

fn read_proc_shr_kb(pid: u32) -> u64 {
    // /proc/<pid>/statm: size resident shared ...
    std::fs::read_to_string(format!("/proc/{}/statm", pid))
        .ok()
        .and_then(|s| s.split_whitespace().nth(2)?.parse::<u64>().ok())
        .unwrap_or(0)
        * 4 // pages to KB
}

fn read_proc_uid(pid: u32) -> u32 {
    std::fs::read_to_string(format!("/proc/{}/status", pid))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn uid_to_name(uid: u32) -> String {
    std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.split(':').nth(2) == Some(&uid.to_string()))
                .map(|l| l.split(':').next().unwrap_or("?").to_string())
        })
        .unwrap_or_else(|| uid.to_string())
}

fn read_proc_cmdline(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{}/cmdline", pid))
        .unwrap_or_default()
        .replace('\0', " ")
        .trim()
        .to_string()
}

fn total_memory_kb() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(1)
}

fn proc_mem_stats(ps: &Option<ProcStat>) -> (char, i32, u64, u64, u32) {
    match ps {
        Some(s) => (
            s.state,
            s.nice,
            s.vsize / 1024,
            s.rss_pages * 4,
            s.num_threads,
        ),
        None => ('?', 0, 0, 0, 0),
    }
}

fn mem_percent(res_kb: u64) -> f32 {
    let total = total_memory_kb();
    if total > 0 {
        (res_kb as f32 / total as f32) * 100.0
    } else {
        0.0
    }
}

fn resolve_user(uid: u32, ps: &Option<ProcStat>) -> String {
    if uid > 0 || ps.is_some() {
        uid_to_name(uid)
    } else {
        "?".into()
    }
}

fn resolve_cmdline(pid: u32, comm: &str) -> String {
    let cmdline = read_proc_cmdline(pid);
    if !cmdline.is_empty() {
        cmdline
    } else {
        comm.to_string()
    }
}

fn enrich_process(pid: u32, comm: &str, dev_name: &str, qp_count: usize) -> ProcessRdmaInfo {
    let ps = read_proc_stat(pid);
    let (state, nice, virt_kb, res_kb, threads) = proc_mem_stats(&ps);

    ProcessRdmaInfo {
        pid,
        dev_name: dev_name.to_string(),
        qp_count,
        user: resolve_user(read_proc_uid(pid), &ps),
        nice,
        state,
        virt_kb,
        res_kb,
        shr_kb: read_proc_shr_kb(pid),
        mem_pct: mem_percent(res_kb),
        threads,
        cmdline: resolve_cmdline(pid, comm),
    }
}

/// Aggregate QPs by (pid, device), enriched with /proc data.
pub fn aggregate_by_process(qps: &[QpInfo]) -> Vec<ProcessRdmaInfo> {
    use std::collections::HashMap;
    let mut counts: HashMap<(u32, String), (&QpInfo, usize)> = HashMap::new();
    for qp in qps {
        if qp.pid == 0 {
            continue;
        }
        let key = (qp.pid, qp.dev_name.clone());
        let entry = counts.entry(key).or_insert((qp, 0));
        entry.1 += 1;
    }
    let mut result: Vec<ProcessRdmaInfo> = counts
        .into_values()
        .map(|(qp, count)| enrich_process(qp.pid, &qp.comm, &qp.dev_name, count))
        .collect();
    result.sort_by(|a, b| b.qp_count.cmp(&a.qp_count).then(a.pid.cmp(&b.pid)));
    result
}
