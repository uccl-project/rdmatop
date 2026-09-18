use crate::stat::PortStat;
use crate::trace::PortMetrics;

#[derive(Clone, Debug)]
pub struct CounterRate {
    pub name: String,
    pub value: u64,
    pub delta: u64,
    pub rate: f64,
    pub is_bytes: bool,
}

pub fn bytes_to_gbps(bytes_per_sec: f64) -> f64 {
    bytes_per_sec * 8.0 / 1_000_000_000.0
}

fn is_bytes_counter(name: &str) -> bool {
    name.ends_with("_bytes") || name.ends_with("_resp_bytes") || name.ends_with("_recv_bytes")
}

fn counter_delta(name: &str, curr_val: u64, prev: Option<&PortStat>) -> u64 {
    let prev_val = prev.and_then(|stat| stat.counter_value(name)).unwrap_or(0);
    curr_val.saturating_sub(prev_val)
}

fn compute_counter_rate(
    counter_name: &str,
    curr_val: u64,
    prev: Option<&PortStat>,
    elapsed: f64,
) -> CounterRate {
    let delta = counter_delta(counter_name, curr_val, prev);
    CounterRate {
        name: counter_name.to_string(),
        value: curr_val,
        delta,
        rate: delta as f64 / elapsed,
        is_bytes: is_bytes_counter(counter_name),
    }
}

fn rate_per_sec(curr: &PortStat, prev: Option<&PortStat>, name: &str, elapsed: f64) -> f64 {
    let curr_val = curr.counter_value(name).unwrap_or(0);
    counter_delta(name, curr_val, prev) as f64 / elapsed
}

pub fn find_prev<'a>(prev: &'a [PortStat], dev: &str, port: u32) -> Option<&'a PortStat> {
    prev.iter().find(|s| s.dev_name == dev && s.port == port)
}

pub fn counter_rates(curr: &PortStat, prev: Option<&PortStat>, elapsed: f64) -> Vec<CounterRate> {
    curr.counters
        .iter()
        .map(|c| compute_counter_rate(&c.name, c.value, prev, elapsed))
        .collect()
}

pub fn port_metrics(curr: &PortStat, prev: Option<&PortStat>, elapsed: f64) -> PortMetrics {
    let rate = |name| rate_per_sec(curr, prev, name, elapsed);
    PortMetrics {
        dev_name: curr.dev_name.clone(),
        port: curr.port,
        tx_gbps: bytes_to_gbps(rate("tx_bytes")),
        rx_gbps: bytes_to_gbps(rate("rx_bytes")),
        tx_pps: rate("tx_pkts"),
        rx_pps: rate("rx_pkts"),
        rx_drops_per_sec: rate("rx_drops"),
    }
}

fn rates_since_baseline(prev: &[PortStat], curr: &PortStat, elapsed: f64) -> Option<PortMetrics> {
    let baseline = find_prev(prev, &curr.dev_name, curr.port)?;
    Some(port_metrics(curr, Some(baseline), elapsed))
}

pub fn rates_between(prev: &[PortStat], curr: &[PortStat], elapsed: f64) -> Vec<PortMetrics> {
    curr.iter()
        .filter_map(|stat| rates_since_baseline(prev, stat, elapsed))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stat::port_stat;

    #[test]
    fn rates_between_derives_five_metrics_per_port() {
        let prev = vec![port_stat(
            "mlx5_0",
            1,
            &[
                ("tx_bytes", 1_000),
                ("rx_bytes", 0),
                ("tx_pkts", 10),
                ("rx_pkts", 0),
                ("rx_drops", 0),
            ],
        )];
        let curr = vec![port_stat(
            "mlx5_0",
            1,
            &[
                ("tx_bytes", 1_000 + 250_000_000),
                ("rx_bytes", 125_000_000),
                ("tx_pkts", 110),
                ("rx_pkts", 50),
                ("rx_drops", 2),
            ],
        )];
        let out = rates_between(&prev, &curr, 2.0);
        assert_eq!(out.len(), 1);
        let metrics = &out[0];
        assert_eq!(metrics.dev_name, "mlx5_0");
        assert_eq!(metrics.port, 1);
        assert!((metrics.tx_gbps - 1.0).abs() < 1e-9);
        assert!((metrics.rx_gbps - 0.5).abs() < 1e-9);
        assert!((metrics.tx_pps - 50.0).abs() < 1e-9);
        assert!((metrics.rx_pps - 25.0).abs() < 1e-9);
        assert!((metrics.rx_drops_per_sec - 1.0).abs() < 1e-9);
    }

    #[test]
    fn port_without_baseline_is_skipped() {
        let prev = vec![port_stat("efa0", 1, &[("tx_bytes", 0)])];
        let curr = vec![
            port_stat("efa0", 1, &[("tx_bytes", 1_000_000_000)]),
            port_stat("efa1", 1, &[("tx_bytes", 8_000_000_000_000)]),
        ];
        let out = rates_between(&prev, &curr, 1.0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].dev_name, "efa0");
        assert!((out[0].tx_gbps - 8.0).abs() < 1e-9);
    }

    #[test]
    fn missing_counter_reads_as_zero_rate() {
        let prev = vec![port_stat("efa0", 1, &[("tx_bytes", 0)])];
        let curr = vec![port_stat("efa0", 1, &[("tx_bytes", 1_000)])];
        let out = rates_between(&prev, &curr, 1.0);
        assert_eq!(out[0].rx_gbps, 0.0);
        assert_eq!(out[0].rx_drops_per_sec, 0.0);
    }

    #[test]
    fn counter_wrap_does_not_go_negative() {
        let prev = vec![port_stat("efa0", 1, &[("tx_bytes", 100)])];
        let curr = vec![port_stat("efa0", 1, &[("tx_bytes", 50)])];
        let out = rates_between(&prev, &curr, 1.0);
        assert_eq!(out[0].tx_gbps, 0.0);
    }
}
