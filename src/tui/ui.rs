use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table, TableState,
    },
    Frame,
};

use super::app::{
    all_columns, App, CounterRate, PortThroughput, TableColumn, BAR_WIDTH, EXTRA_COUNTERS,
};
use super::theme::ThemeColors;

const HELP_KEYS: &[(&str, &str)] = &[
    ("↑ / k", "Move up"),
    ("↓ / j", "Move down"),
    ("← / →", "Scroll columns"),
    ("Enter", "Toggle detail panel"),
    ("Esc", "Close detail / quit"),
    ("t", "Cycle theme"),
    ("a", "Toggle rolling average"),
    ("+ / =", "Increase avg window (+1s)"),
    ("-", "Decrease avg window (-1s)"),
    ("w", "Set avg window (custom)"),
    ("< / >", "Refresh faster / slower (±0.5s)"),
    ("c", "Configure columns"),
    ("r", "Record Perfetto trace (start/stop)"),
    ("h", "Toggle this help"),
    ("q", "Quit"),
    ("", ""),
    ("", "── Detail mode ──"),
    ("↑ / k", "Scroll up"),
    ("↓ / j", "Scroll down"),
    ("", "Scroll past end → next device"),
    ("", "Scroll past top  → prev device"),
];

const RDMA_LINK_GBPS: f64 = 100.0;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let tc = app.theme.colors();

    if tc.bg != ratatui::style::Color::Reset {
        frame.render_widget(
            Block::default().style(Style::default().bg(tc.bg)),
            frame.area(),
        );
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, app, chunks[0], &tc);
    draw_body(frame, app, chunks[1], &tc);
    draw_status_bar(frame, app, chunks[2], &tc);

    if app.show_help {
        draw_help_popup(frame, &tc);
    }
    if app.show_window_input {
        draw_window_input_popup(frame, app, &tc);
    }
    if app.show_column_picker {
        draw_column_picker(frame, app, &tc);
    }
}

fn draw_body(frame: &mut Frame, app: &mut App, area: Rect, tc: &ThemeColors) {
    if app.show_detail {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        draw_table(frame, app, split[0], tc);
        draw_detail(frame, app, split[1], tc);
    } else {
        draw_table(frame, app, area, tc);
    }
}

fn header_line1(app: &App, tc: &ThemeColors) -> Line<'static> {
    Line::from(vec![
        styled(" rdmatop ", tc.accent, true),
        styled(
            &format!(
                "- {} │ {} │ load average: {}",
                app.sysinfo.hostname, app.sysinfo.uptime, app.sysinfo.load_avg
            ),
            tc.muted,
            false,
        ),
    ])
}

fn header_line2(app: &App, tc: &ThemeColors) -> Line<'static> {
    let display = app.display_throughputs();
    let n = display.len();
    let total_tx: f64 = display.iter().map(|t| t.tx_gbps).sum();
    let total_rx: f64 = display.iter().map(|t| t.rx_gbps).sum();
    let total_drops: f64 = display.iter().map(|t| t.rx_drops_per_sec).sum();
    let drop_color = if total_drops > 0.0 {
        tc.error
    } else {
        tc.muted
    };

    let avg_label = if app.show_rolling_avg {
        format!(
            " │ avg:{}s({}/{})",
            app.rolling_avg.window_secs,
            app.rolling_avg.sample_count(),
            app.rolling_avg.window_secs,
        )
    } else {
        String::new()
    };

    let status = format!(
        " │ refresh: {:.1}s │ theme: {}",
        app.refresh_interval.as_secs_f64(),
        app.theme.label()
    );

    Line::from(vec![
        styled(
            &format!(" RDMA: {} device{}", n, if n == 1 { "" } else { "s" }),
            tc.fg,
            false,
        ),
        styled(" │ TX: ", tc.muted, false),
        styled(&format!("{:.2} Gbps", total_tx), tc.good, false),
        styled(" │ RX: ", tc.muted, false),
        styled(&format!("{:.2} Gbps", total_rx), tc.good, false),
        styled(" │ Drops: ", tc.muted, false),
        styled(&format!("{:.0}/s", total_drops), drop_color, false),
        styled(&status, tc.muted, false),
        styled(&avg_label, tc.accent, false),
    ])
}

fn cpu_bar(pct: f32, width: usize, tc: &ThemeColors) -> Vec<Span<'static>> {
    let filled = ((pct / 100.0) * width as f32).round() as usize;
    let empty = width.saturating_sub(filled);
    let color = if pct > 80.0 {
        tc.error
    } else if pct > 50.0 {
        tc.warning
    } else {
        tc.good
    };
    vec![
        styled("[", tc.muted, false),
        styled(&"|".repeat(filled), color, false),
        styled(&" ".repeat(empty), tc.muted, false),
        styled(&format!("{:>5.1}%]", pct), color, false),
    ]
}

fn mem_bar(used: u64, total: u64, pct: f32, width: usize, tc: &ThemeColors) -> Vec<Span<'static>> {
    let filled = ((pct / 100.0) * width as f32).round() as usize;
    let empty = width.saturating_sub(filled);
    let color = if pct > 80.0 {
        tc.error
    } else if pct > 50.0 {
        tc.warning
    } else {
        tc.good
    };
    let label = if total >= 1024 {
        format!("{:.1}/{:.1}G]", used as f64 / 1024.0, total as f64 / 1024.0)
    } else {
        format!("{}/{}M]", used, total)
    };
    vec![
        styled("[", tc.muted, false),
        styled(&"|".repeat(filled), color, false),
        styled(&" ".repeat(empty), tc.muted, false),
        styled(&label, color, false),
    ]
}

fn header_line3(app: &App, tc: &ThemeColors) -> Line<'static> {
    let s = &app.sysinfo;
    let mut spans = vec![styled(" CPU ", tc.muted, false)];
    spans.extend(cpu_bar(s.cpu_pct, 20, tc));
    spans.push(styled("  Mem ", tc.muted, false));
    spans.extend(mem_bar(s.mem_used_mb, s.mem_total_mb, s.mem_pct, 20, tc));
    spans.push(styled("  Net ", tc.muted, false));
    spans.push(styled(
        &format!(
            "↓{}/s ↑{}/s",
            fmt_bytes_short(s.net.rx_bytes_per_sec),
            fmt_bytes_short(s.net.tx_bytes_per_sec),
        ),
        tc.fg,
        false,
    ));
    Line::from(spans)
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect, tc: &ThemeColors) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(tc.border));
    let lines = vec![
        header_line1(app, tc),
        header_line2(app, tc),
        header_line3(app, tc),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn gbps_bar(gbps: f64, link_gbps: Option<f64>) -> String {
    // Scale to the port's line rate; fall back to a default when unknown.
    let max = link_gbps.filter(|&r| r > 0.0).unwrap_or(RDMA_LINK_GBPS);
    let ratio = (gbps / max).clamp(0.0, 1.0);
    let filled = (ratio * BAR_WIDTH as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(BAR_WIDTH - filled))
}

fn column_cell(col: &TableColumn, t: &PortThroughput, tc: &ThemeColors) -> Cell<'static> {
    match col {
        TableColumn::Device => Cell::from(t.dev_name.clone()).style(Style::default().fg(tc.fg)),
        TableColumn::Port => {
            let port_text = t.port_label.clone().unwrap_or_else(|| t.port.to_string());
            Cell::from(port_text).style(Style::default().fg(tc.muted))
        }
        TableColumn::TxBar => Cell::from(gbps_bar(t.tx_gbps, t.link_gbps))
            .style(Style::default().fg(gbps_color(t.tx_gbps, tc))),
        TableColumn::TxGbps => Cell::from(format!("{:.2}", t.tx_gbps))
            .style(Style::default().fg(gbps_color(t.tx_gbps, tc))),
        TableColumn::RxBar => Cell::from(gbps_bar(t.rx_gbps, t.link_gbps))
            .style(Style::default().fg(gbps_color(t.rx_gbps, tc))),
        TableColumn::RxGbps => Cell::from(format!("{:.2}", t.rx_gbps))
            .style(Style::default().fg(gbps_color(t.rx_gbps, tc))),
        TableColumn::TxPps => {
            Cell::from(format_pps(t.tx_pkts_per_sec)).style(Style::default().fg(tc.fg))
        }
        TableColumn::RxPps => {
            Cell::from(format_pps(t.rx_pkts_per_sec)).style(Style::default().fg(tc.fg))
        }
        TableColumn::Drops => {
            let c = if t.rx_drops_per_sec > 0.0 {
                tc.error
            } else {
                tc.muted
            };
            Cell::from(format!("{:.0}", t.rx_drops_per_sec)).style(Style::default().fg(c))
        }
        TableColumn::Counter(name) => {
            let rate = t
                .counter_rates
                .iter()
                .find(|r| &r.name == name)
                .map(|r| r.rate)
                .unwrap_or(0.0);
            let is_bytes = t
                .counter_rates
                .iter()
                .find(|r| &r.name == name)
                .map(|r| r.is_bytes)
                .unwrap_or(false);
            let text = if is_bytes {
                format_bytes(rate)
            } else {
                format_rate(rate)
            };
            let color = if rate > 0.0 { tc.fg } else { tc.muted };
            Cell::from(text).style(Style::default().fg(color))
        }
    }
}

fn draw_table(frame: &mut Frame, app: &mut App, area: Rect, tc: &ThemeColors) {
    let display = app.display_throughputs().to_vec();
    let title = if app.show_rolling_avg {
        format!(" RDMA Throughput (avg {}s) ", app.rolling_avg.window_secs)
    } else {
        " RDMA Throughput ".to_string()
    };

    // In detail mode, use default columns with no scrolling (original behavior).
    // In normal mode, use configured columns with horizontal scroll.
    let default_cols = super::app::default_columns();
    let (cols_to_render, show_scrollbars) = if app.show_detail {
        (default_cols.iter().collect::<Vec<_>>(), false)
    } else {
        let all_cols = &app.columns;
        let avail = area.width.saturating_sub(4) as usize;

        let visible: Vec<&super::app::TableColumn> = all_cols
            .iter()
            .skip(app.h_scroll)
            .scan(0usize, |used, col| {
                let sep = if *used > 0 { 1 } else { 0 };
                let w = col.width() as usize + sep;
                if *used + w <= avail {
                    *used += w;
                    Some(col)
                } else {
                    None
                }
            })
            .collect();

        // Compute max horizontal scroll
        let mut max_offset = 0;
        for start in 0..all_cols.len() {
            let total_w: usize = all_cols[start..]
                .iter()
                .map(|c| c.width() as usize + 1)
                .sum();
            if total_w > avail {
                max_offset = start + 1;
            } else {
                break;
            }
        }
        app.h_scroll_max = max_offset;
        if app.h_scroll > app.h_scroll_max {
            app.h_scroll = app.h_scroll_max;
        }

        (visible, true)
    };

    let header = Row::new(cols_to_render.iter().map(|c| c.label()).collect::<Vec<_>>())
        .style(
            Style::default()
                .fg(tc.header_fg)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);

    let rows: Vec<Row> = display
        .iter()
        .map(|t| {
            Row::new(
                cols_to_render
                    .iter()
                    .map(|c| column_cell(c, t, tc))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();

    let widths: Vec<Constraint> = cols_to_render
        .iter()
        .map(|c| Constraint::Length(c.width()))
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(tc.border))
                .title(title)
                .title_style(Style::default().fg(tc.accent)),
        )
        .row_highlight_style(
            Style::default()
                .bg(tc.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = TableState::default();
    if !display.is_empty() {
        let viewport = area.height.saturating_sub(4) as usize; // borders + header
        if viewport > 0 {
            if app.selected_row >= app.table_offset + viewport {
                app.table_offset = app.selected_row + 1 - viewport;
            } else if app.selected_row < app.table_offset {
                app.table_offset = app.selected_row;
            }
        }
        state.select(Some(app.selected_row));
        *state.offset_mut() = app.table_offset;
    }
    frame.render_stateful_widget(table, area, &mut state);

    if !show_scrollbars {
        return;
    }

    // Vertical scrollbar (right side, inside border)
    if display.len() > area.height.saturating_sub(4) as usize {
        let mut v_scroll = ScrollbarState::new(display.len()).position(app.selected_row);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("▐")
                .track_symbol(Some("│"))
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .thumb_style(Style::default().fg(tc.accent))
                .track_style(Style::default().fg(tc.border)),
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut v_scroll,
        );
    }

    // Horizontal scrollbar (bottom, inside border)
    let all_cols = &app.columns;
    if all_cols.len() > cols_to_render.len() || app.h_scroll > 0 {
        let mut h_scroll = ScrollbarState::new(all_cols.len()).position(app.h_scroll);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
                .thumb_symbol("▬")
                .track_symbol(Some("─"))
                .begin_symbol(Some("◀"))
                .end_symbol(Some("▶"))
                .thumb_style(Style::default().fg(tc.accent))
                .track_style(Style::default().fg(tc.border)),
            area.inner(Margin {
                vertical: 0,
                horizontal: 1,
            }),
            &mut h_scroll,
        );
    }
}

fn sparkline_str(data: &[f64], width: usize) -> String {
    const BARS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = data.iter().cloned().fold(0.0f64, f64::max).max(0.01);
    let start = data.len().saturating_sub(width);
    let mut s = String::with_capacity(width);
    for &v in &data[start..] {
        let idx = ((v / max) * 8.0).round() as usize;
        s.push(BARS[idx.min(8)]);
    }
    // Pad if not enough data
    while s.chars().count() < width {
        s.insert(0, ' ');
    }
    s
}

fn build_detail_lines(
    t: &PortThroughput,
    procs: &[&crate::stat::ProcessRdmaInfo],
    history: Option<&super::app::DeviceHistory>,
    tc: &ThemeColors,
    show_avg: bool,
    avg_window: usize,
) -> Vec<Line<'static>> {
    let mut lines = build_device_header(t, history, tc, show_avg, avg_window);
    append_active_counters(&mut lines, t, tc);
    append_process_table(&mut lines, procs, tc);
    lines
}

/// Build the per-lane detail panel for a NVLink GPU row.
///
/// Layout:
///   Device: nvidiaN  [NVLink]  active/total active
///   TX/RX aggregate Gbps with sparkline
///   Lane table header (Lane, State, Ver, TX MB/s, RX MB/s, Remote)
///   One row per link, including inactive lanes
///   Summed error counters (replay/recovery/crc)
#[cfg(feature = "nvlink")]
fn build_nvlink_detail_lines(
    t: &PortThroughput,
    meta: &super::app::NvLinkThroughputMeta,
    history: Option<&super::app::DeviceHistory>,
    tc: &ThemeColors,
    show_avg: bool,
    avg_window: usize,
) -> Vec<Line<'static>> {
    let spark_w = 30;
    let (tx_spark, rx_spark) = match history {
        Some(h) => (sparkline_str(&h.tx, spark_w), sparkline_str(&h.rx, spark_w)),
        None => (" ".repeat(spark_w), " ".repeat(spark_w)),
    };
    let avg_label = if show_avg {
        format!("  [avg {}s]", avg_window)
    } else {
        String::new()
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header: Device: nvidiaN  [NVLink]  active/total active
    lines.push(Line::from(vec![
        styled(" Device: ", tc.muted, false),
        styled(&t.dev_name, tc.accent, true),
        styled("  [NVLink]  ", tc.accent, false),
        styled(
            &format!("{}/{} active", meta.active_links, meta.links.len()),
            tc.fg,
            false,
        ),
        styled(&avg_label, tc.accent, false),
    ]));

    // Aggregate TX line with sparkline.
    lines.push(Line::from(vec![
        styled(" TX: ", tc.muted, false),
        styled(&format!("{:.2} Gbps ", t.tx_gbps), tc.good, false),
        styled(&tx_spark, tc.good, false),
    ]));

    // Aggregate RX line with sparkline.
    lines.push(Line::from(vec![
        styled(" RX: ", tc.muted, false),
        styled(&format!("{:.2} Gbps ", t.rx_gbps), tc.accent, false),
        styled(&rx_spark, tc.accent, false),
    ]));

    lines.push(Line::from(""));

    // Muted note explaining that the per-lane TX/RX columns below show the
    // GPU-wide aggregate, since NVML does not expose per-lane throughput.
    lines.push(Line::from(styled(
        " Per-lane throughput is not available from NVML on some drivers; values below show either per-lane rate or the GPU aggregate.",
        tc.muted,
        false,
    )));

    // Lane table header.
    lines.push(Line::from(vec![styled(
        " Lane  State     Ver   TX MB/s   RX MB/s  Remote",
        tc.header_fg,
        true,
    )]));

    // One row per link (active + inactive). Render per-lane throughput if available,
    // otherwise fallback to the GPU aggregate rate (converted from Gbps to MB/s).
    for link in &meta.links {
        let link_tx_mb_s = match link.tx_bytes {
            Some(bytes) => bytes as f64 / 1_000_000.0,
            None => t.tx_gbps * 1000.0 / 8.0,
        };
        let link_rx_mb_s = match link.rx_bytes {
            Some(bytes) => bytes as f64 / 1_000_000.0,
            None => t.rx_gbps * 1000.0 / 8.0,
        };

        let state_label = if link.is_active {
            "enabled"
        } else {
            "disabled"
        };
        let state_color = if link.is_active { tc.good } else { tc.muted };
        let ver_label = link
            .version
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string());
        let remote_label = match (&link.remote_pci_bdf, &link.remote_device_type) {
            (Some(bdf), ty) => format!("{} {}", ty.label(), bdf),
            (None, ty) => match ty {
                crate::nvlink::RemoteDeviceType::Unknown => "-".to_string(),
                _ => ty.label().to_string(),
            },
        };
        lines.push(Line::from(vec![
            styled(&format!(" {:>4}", link.link_id), tc.accent, false),
            styled(&format!("  {:<8}", state_label), state_color, false),
            styled(&format!("  {:>3}", ver_label), tc.fg, false),
            styled(&format!("  {:>6.1}", link_tx_mb_s), tc.fg, false),
            styled(&format!("  {:>6.1}", link_rx_mb_s), tc.fg, false),
            styled(&format!("  {}", remote_label), tc.muted, false),
        ]));
    }

    lines.push(Line::from(""));

    // Errors line: sum per-link counters across all lanes. None -> 0.
    let mut crc: u64 = 0;
    let mut replay: u64 = 0;
    let mut recovery: u64 = 0;
    for link in &meta.links {
        crc = crc.saturating_add(link.crc_error_count.unwrap_or(0));
        replay = replay.saturating_add(link.replay_error_count.unwrap_or(0));
        recovery = recovery.saturating_add(link.recovery_error_count.unwrap_or(0));
    }
    lines.push(Line::from(vec![
        styled(" Errors: ", tc.muted, false),
        styled(&format!("replay={}", replay), tc.warning, false),
        styled("  ", tc.muted, false),
        styled(&format!("recovery={}", recovery), tc.warning, false),
        styled("  ", tc.muted, false),
        styled(&format!("crc={}", crc), tc.warning, false),
    ]));

    lines
}

/// Build the per-link detail panel for an XGMI GPU row.
/// Layout mirrors the NVLink pane: header, TX/RX sparklines, a link table
/// (Link, State, Gbps, TX MB/s, RX MB/s, Remote), then XGMI_WAFL ECC counts.
#[cfg(feature = "xgmi")]
fn build_xgmi_detail_lines(
    t: &PortThroughput,
    meta: &super::app::XgmiThroughputMeta,
    history: Option<&super::app::DeviceHistory>,
    tc: &ThemeColors,
    show_avg: bool,
    avg_window: usize,
) -> Vec<Line<'static>> {
    let spark_w = 30;
    let (tx_spark, rx_spark) = match history {
        Some(h) => (sparkline_str(&h.tx, spark_w), sparkline_str(&h.rx, spark_w)),
        None => (" ".repeat(spark_w), " ".repeat(spark_w)),
    };
    let avg_label = if show_avg {
        format!("  [avg {}s]", avg_window)
    } else {
        String::new()
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        styled(" Device: ", tc.muted, false),
        styled(&t.dev_name, tc.accent, true),
        styled("  [XGMI]  ", tc.accent, false),
        styled(
            &format!("{}/{} active", meta.active_links, meta.links.len()),
            tc.fg,
            false,
        ),
        styled(&avg_label, tc.accent, false),
    ]));

    lines.push(Line::from(vec![
        styled(" TX: ", tc.muted, false),
        styled(&format!("{:.2} Gbps ", t.tx_gbps), tc.good, false),
        styled(&tx_spark, tc.good, false),
    ]));
    lines.push(Line::from(vec![
        styled(" RX: ", tc.muted, false),
        styled(&format!("{:.2} Gbps ", t.rx_gbps), tc.accent, false),
        styled(&rx_spark, tc.accent, false),
    ]));

    lines.push(Line::from(""));

    lines.push(Line::from(vec![styled(
        " Link  State   Gbps   TX MB/s   RX MB/s  Remote",
        tc.header_fg,
        true,
    )]));

    for link in &meta.links {
        let tx_mb_s = link.tx_bytes.unwrap_or(0) as f64 / 1_000_000.0;
        let rx_mb_s = link.rx_bytes.unwrap_or(0) as f64 / 1_000_000.0;
        let state_label = if link.is_active { "up" } else { "down" };
        let state_color = if link.is_active { tc.good } else { tc.muted };
        let gbps_label = link
            .speed_gbps
            .map(|v| format!("{:.0}", v))
            .unwrap_or_else(|| "-".to_string());
        // XGMI peers are always GPUs; show a bare "-" when the BDF is unknown.
        let remote_label = link
            .remote_pci_bdf
            .as_ref()
            .map(|bdf| format!("GPU {}", bdf))
            .unwrap_or_else(|| "-".to_string());
        lines.push(Line::from(vec![
            styled(&format!(" {:>4}", link.link_id), tc.accent, false),
            styled(&format!("  {:<6}", state_label), state_color, false),
            styled(&format!("  {:>4}", gbps_label), tc.fg, false),
            styled(&format!("  {:>7.1}", tx_mb_s), tc.fg, false),
            styled(&format!("  {:>7.1}", rx_mb_s), tc.fg, false),
            styled(&format!("  {}", remote_label), tc.muted, false),
        ]));
    }

    lines.push(Line::from(""));

    // XGMI_WAFL ECC counters come through counter_rates (per-GPU values).
    let counter = |name: &str| -> u64 {
        t.counter_rates
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.value)
            .unwrap_or(0)
    };
    lines.push(Line::from(vec![
        styled(" Errors: ", tc.muted, false),
        styled(
            &format!("wafl_ce={}", counter("xgmi_wafl_ce")),
            tc.warning,
            false,
        ),
        styled("  ", tc.muted, false),
        styled(
            &format!("wafl_ue={}", counter("xgmi_wafl_ue")),
            tc.warning,
            false,
        ),
    ]));

    lines
}

fn build_device_header(
    t: &PortThroughput,
    history: Option<&super::app::DeviceHistory>,
    tc: &ThemeColors,
    show_avg: bool,
    avg_window: usize,
) -> Vec<Line<'static>> {
    let spark_w = 30;
    let (tx_spark, rx_spark) = match history {
        Some(h) => (sparkline_str(&h.tx, spark_w), sparkline_str(&h.rx, spark_w)),
        None => (" ".repeat(spark_w), " ".repeat(spark_w)),
    };
    let mode_label = if show_avg {
        format!("  [avg {}s]", avg_window)
    } else {
        String::new()
    };
    vec![
        Line::from(vec![
            styled(" Device: ", tc.muted, false),
            styled(&format!("{}/{}", t.dev_name, t.port), tc.accent, true),
            styled(&mode_label, tc.accent, false),
        ]),
        Line::from(vec![
            styled(" TX: ", tc.muted, false),
            styled(&format!("{:.2} Gbps ", t.tx_gbps), tc.good, false),
            styled(&tx_spark, tc.good, false),
        ]),
        Line::from(vec![
            styled(" RX: ", tc.muted, false),
            styled(&format!("{:.2} Gbps ", t.rx_gbps), tc.accent, false),
            styled(&rx_spark, tc.accent, false),
        ]),
        Line::from(""),
    ]
}

fn append_active_counters(lines: &mut Vec<Line<'static>>, t: &PortThroughput, tc: &ThemeColors) {
    // Show every counter that's actually carrying signal:
    //   - currently changing (delta/rate > 0), or
    //   - has accumulated some non-zero value, or
    //   - is in the curated EXTRA_COUNTERS list (always shown for visibility).
    // This surfaces useful Mellanox metrics like rnr_nak_retry_err, link_downed,
    // port_xmit_discards, etc. that the previous allow-list filter hid.
    let mut counters: Vec<_> = t
        .counter_rates
        .iter()
        .filter(|r| r.value > 0 || EXTRA_COUNTERS.contains(&r.name.as_str()))
        .collect();
    // Most useful first: active rates, then accumulated, then alphabetical.
    counters.sort_by(|a, b| {
        b.rate
            .partial_cmp(&a.rate)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.value.cmp(&a.value))
            .then(a.name.cmp(&b.name))
    });
    if !counters.is_empty() {
        for r in &counters {
            lines.push(counter_rate_line(r, tc));
        }
        lines.push(Line::from(""));
    }
}

const PROC_HEADER: &str =
    "  PID     USER     NI S     VIRT      RES    SHR  MEM%   QPs  THR COMMAND";

fn append_process_table(
    lines: &mut Vec<Line<'static>>,
    procs: &[&crate::stat::ProcessRdmaInfo],
    tc: &ThemeColors,
) {
    lines.push(Line::from(vec![styled(PROC_HEADER, tc.header_fg, true)]));
    if procs.is_empty() {
        lines.push(Line::from(styled("  (no RDMA processes)", tc.muted, false)));
    } else {
        for p in procs {
            lines.push(process_line(p, tc));
        }
    }
}

fn process_line(p: &crate::stat::ProcessRdmaInfo, tc: &ThemeColors) -> Line<'static> {
    let state_color = match p.state {
        'R' => tc.good,
        'S' | 'I' => tc.muted,
        'D' => tc.warning,
        'Z' | 'T' => tc.error,
        _ => tc.fg,
    };
    Line::from(vec![
        styled(&format!("  {:<7}", p.pid), tc.accent, false),
        styled(&format!(" {:<8}", truncate(&p.user, 8)), tc.fg, false),
        styled(&format!(" {:<2}", p.nice), tc.muted, false),
        styled(&format!(" {:>1}", p.state), state_color, false),
        styled(&format!(" {:>8}", fmt_mem_kb(p.virt_kb)), tc.fg, false),
        styled(&format!(" {:>8}", fmt_mem_kb(p.res_kb)), tc.good, false),
        styled(&format!(" {:>6}", fmt_mem_kb(p.shr_kb)), tc.fg, false),
        styled(&format!(" {:>4.1}", p.mem_pct), tc.fg, false),
        styled(&format!(" {:>5}", p.qp_count), tc.accent, false),
        styled(&format!(" {:>4}", p.threads), tc.muted, false),
        styled(&format!(" {}", truncate(&p.cmdline, 40)), tc.fg, false),
    ])
}

/// Pick the right detail pane builder for the selected row: NVLink pane,
/// XGMI pane, or the generic RDMA device pane.
fn build_selected_detail_lines(
    t: &PortThroughput,
    procs: &[&crate::stat::ProcessRdmaInfo],
    history: Option<&super::app::DeviceHistory>,
    tc: &ThemeColors,
    show_avg: bool,
    avg_window: usize,
) -> Vec<Line<'static>> {
    #[cfg(feature = "nvlink")]
    if let Some(ref meta) = t.nvlink {
        return build_nvlink_detail_lines(t, meta, history, tc, show_avg, avg_window);
    }
    #[cfg(feature = "xgmi")]
    if let Some(ref meta) = t.xgmi {
        return build_xgmi_detail_lines(t, meta, history, tc, show_avg, avg_window);
    }
    build_detail_lines(t, procs, history, tc, show_avg, avg_window)
}

fn draw_detail(frame: &mut Frame, app: &mut App, area: Rect, tc: &ThemeColors) {
    let display = app.display_throughputs();
    let t = match display.get(app.selected_row).cloned() {
        Some(t) => t,
        None => {
            frame.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(tc.border))
                    .title(" Detail "),
                area,
            );
            return;
        }
    };

    let history = app.history.get(&t.dev_name);
    let procs = app.selected_device_processes();

    let lines = build_selected_detail_lines(
        &t,
        &procs,
        history,
        tc,
        app.show_rolling_avg,
        app.rolling_avg.window_secs,
    );

    let visible = area.height.saturating_sub(2);
    app.detail_max_scroll = (lines.len() as u16).saturating_sub(visible);

    let title = if app.show_rolling_avg {
        format!(" {} (avg {}s) ", t.dev_name, app.rolling_avg.window_secs)
    } else {
        format!(" {} ", t.dev_name)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(tc.border))
        .title(title)
        .title_style(Style::default().fg(tc.accent).add_modifier(Modifier::BOLD));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((app.detail_scroll, 0)),
        area,
    );
}

fn counter_rate_line(r: &CounterRate, tc: &ThemeColors) -> Line<'static> {
    let rate_str = if r.is_bytes {
        format_bytes(r.rate)
    } else {
        format_rate(r.rate)
    };
    let color = counter_color(r, tc);

    Line::from(vec![
        Span::styled(format!("  {:<35}", r.name), Style::default().fg(tc.fg)),
        Span::styled(format!("{:>12}", rate_str), Style::default().fg(color)),
        Span::styled(format!("  Δ {}", r.delta), Style::default().fg(tc.muted)),
    ])
}

fn counter_color(r: &CounterRate, tc: &ThemeColors) -> ratatui::style::Color {
    let is_error = r.name.contains("err") || r.name.contains("drop");
    let is_warn = r.name.contains("retrans")
        || r.name.contains("unresponsive")
        || r.name.contains("impaired");

    match (r.delta > 0, is_error, is_warn) {
        (true, true, _) => tc.error,
        (true, _, true) => tc.warning,
        (true, _, _) => tc.good,
        _ => tc.muted,
    }
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect, tc: &ThemeColors) {
    let hint = if app.show_detail {
        "Enter/Esc:close"
    } else {
        "Enter:detail"
    };
    let avg_hint = if app.show_rolling_avg {
        format!(
            "  a:avg[ON {}s]  +/-:window  w:set",
            app.rolling_avg.window_secs
        )
    } else {
        "  a:avg  w:set".to_string()
    };
    let keys = format!(
        " ↑↓/jk:nav  {}  t:theme  <>:refresh  r:rec  h:help  q:quit{}",
        hint, avg_hint
    );
    let mode_style = Style::default()
        .fg(tc.status_fg)
        .bg(tc.status_bg)
        .add_modifier(Modifier::BOLD);
    let mode = Span::styled(" NORMAL ", mode_style);
    let keys = Span::styled(keys, Style::default().fg(tc.muted));
    let mut spans = vec![mode];
    if let Some((secs, samples)) = app.recording_progress() {
        spans.push(Span::styled(
            format!(" ● REC {}:{:02} ({}) ", secs / 60, secs % 60, samples),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    } else if let Some(msg) = &app.record_status {
        spans.push(Span::styled(
            format!(" {} ", msg),
            Style::default().fg(tc.fg),
        ));
    }
    spans.push(keys);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_help_popup(frame: &mut Frame, tc: &ThemeColors) {
    let area = frame.area();
    let w = 50.min(area.width.saturating_sub(4));
    let h = 18.min(area.height.saturating_sub(4));
    let popup = centered_rect(area, w, h);

    frame.render_widget(Clear, popup);

    let lines: Vec<Line> = HELP_KEYS
        .iter()
        .map(|(key, desc)| help_line(key, desc, tc))
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(tc.accent))
        .title(" Help (h/Esc to close) ")
        .title_style(Style::default().fg(tc.accent).add_modifier(Modifier::BOLD));

    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn draw_window_input_popup(frame: &mut Frame, app: &App, tc: &ThemeColors) {
    let area = frame.area();
    let w = 40.min(area.width.saturating_sub(4));
    let h = 5.min(area.height.saturating_sub(4));
    let popup = centered_rect(area, w, h);

    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(vec![
            styled(" Window (1-300s): ", tc.muted, false),
            styled(&app.window_input_buf, tc.accent, true),
            styled("▏", tc.accent, false),
        ]),
        Line::from(styled(" Enter:confirm  Esc:cancel", tc.muted, false)),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(tc.accent))
        .title(" Set Avg Window ")
        .title_style(Style::default().fg(tc.accent).add_modifier(Modifier::BOLD));

    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn draw_column_picker(frame: &mut Frame, app: &App, tc: &ThemeColors) {
    let all = all_columns();
    let area = frame.area();
    let w = 45.min(area.width.saturating_sub(4));
    let h = ((all.len() + 4) as u16).min(area.height.saturating_sub(4));
    let popup = centered_rect(area, w, h);

    frame.render_widget(Clear, popup);

    let lines: Vec<Line> = all
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let enabled = app.columns.contains(col);
            let marker = if enabled { "[x]" } else { "[ ]" };
            let cursor = if i == app.column_picker_cursor {
                "▶"
            } else {
                " "
            };
            let color = if i == app.column_picker_cursor {
                tc.accent
            } else if enabled {
                tc.fg
            } else {
                tc.muted
            };
            Line::from(styled(
                &format!(" {} {} {}", cursor, marker, col.label()),
                color,
                i == app.column_picker_cursor,
            ))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(tc.accent))
        .title(" Columns (Space:toggle  Esc:close) ")
        .title_style(Style::default().fg(tc.accent).add_modifier(Modifier::BOLD));

    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn help_line(key: &str, desc: &str, tc: &ThemeColors) -> Line<'static> {
    if key.is_empty() {
        Line::from(styled(&format!("  {}", desc), tc.group_title, false))
    } else {
        Line::from(vec![
            styled(&format!("  {:<14}", key), tc.accent, false),
            styled(desc, tc.fg, false),
        ])
    }
}

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    )
}

fn styled(text: &str, color: ratatui::style::Color, bold: bool) -> Span<'static> {
    let s = Style::default().fg(color);
    Span::styled(
        text.to_string(),
        if bold {
            s.add_modifier(Modifier::BOLD)
        } else {
            s
        },
    )
}

fn fmt_bytes_short(bps: f64) -> String {
    if bps >= 1_000_000_000.0 {
        format!("{:.1}G", bps / 1_000_000_000.0)
    } else if bps >= 1_000_000.0 {
        format!("{:.1}M", bps / 1_000_000.0)
    } else if bps >= 1_000.0 {
        format!("{:.1}K", bps / 1_000.0)
    } else {
        format!("{:.0}B", bps)
    }
}

fn format_bytes(bps: f64) -> String {
    if bps >= 1_073_741_824.0 {
        format!("{:.2} GB/s", bps / 1_073_741_824.0)
    } else if bps >= 1_048_576.0 {
        format!("{:.2} MB/s", bps / 1_048_576.0)
    } else if bps >= 1024.0 {
        format!("{:.2} KB/s", bps / 1024.0)
    } else {
        format!("{:.0} B/s", bps)
    }
}

fn format_pps(pps: f64) -> String {
    if pps >= 1_000_000.0 {
        format!("{:.2}M", pps / 1_000_000.0)
    } else if pps >= 1_000.0 {
        format!("{:.1}K", pps / 1_000.0)
    } else {
        format!("{:.0}", pps)
    }
}

fn format_rate(rate: f64) -> String {
    if rate >= 1_000_000.0 {
        format!("{:.2}M/s", rate / 1_000_000.0)
    } else if rate >= 1_000.0 {
        format!("{:.1}K/s", rate / 1_000.0)
    } else {
        format!("{:.1}/s", rate)
    }
}

fn gbps_color(gbps: f64, tc: &ThemeColors) -> ratatui::style::Color {
    if gbps >= 10.0 {
        tc.good
    } else if gbps >= 1.0 {
        tc.warning
    } else {
        tc.muted
    }
}

fn fmt_mem_kb(kb: u64) -> String {
    if kb >= 1_048_576 {
        format!("{:.1}G", kb as f64 / 1_048_576.0)
    } else if kb >= 1024 {
        format!("{:.0}M", kb as f64 / 1024.0)
    } else {
        format!("{}K", kb)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

#[cfg(all(test, feature = "nvlink"))]
mod nvlink_detail_tests {
    use super::*;
    use crate::nvlink::{LinkSnapshot, RemoteDeviceType};
    use crate::tui::app::{CounterRate, NvLinkThroughputMeta};
    use crate::tui::theme::Theme;

    #[allow(clippy::too_many_arguments)]
    fn make_link(
        id: u32,
        active: bool,
        remote_type: RemoteDeviceType,
        remote_bdf: Option<&str>,
        tx_bytes: Option<u64>,
        rx_bytes: Option<u64>,
        crc: Option<u64>,
        replay: Option<u64>,
        recovery: Option<u64>,
        version: Option<u32>,
    ) -> LinkSnapshot {
        LinkSnapshot {
            link_id: id,
            is_active: active,
            version,
            speed_gbps: if active { Some(50.0) } else { None },
            remote_device_type: remote_type,
            remote_pci_bdf: remote_bdf.map(|s| s.to_string()),
            tx_bytes,
            rx_bytes,
            crc_error_count: crc,
            replay_error_count: replay,
            recovery_error_count: recovery,
        }
    }

    /// Build a synthetic `PortThroughput` whose `counter_rates` contain one
    /// `nvlink_tx_l<N>` / `nvlink_rx_l<N>` pair per provided link, with the
    /// supplied `tx_rate` / `rx_rate` (bytes/sec) values. Returned tuple is
    /// `(PortThroughput, NvLinkThroughputMeta)` — they share the same `links`
    /// list so the detail renderer sees a consistent view.
    fn make_port(
        dev_name: &str,
        links: Vec<LinkSnapshot>,
        tx_rate_bps: f64,
        rx_rate_bps: f64,
    ) -> (PortThroughput, NvLinkThroughputMeta) {
        let counter_rates: Vec<CounterRate> = links
            .iter()
            .flat_map(|l| {
                vec![
                    CounterRate {
                        name: format!("nvlink_tx_l{}", l.link_id),
                        value: l.tx_bytes.unwrap_or(0),
                        delta: 0,
                        rate: tx_rate_bps,
                        is_bytes: true,
                    },
                    CounterRate {
                        name: format!("nvlink_rx_l{}", l.link_id),
                        value: l.rx_bytes.unwrap_or(0),
                        delta: 0,
                        rate: rx_rate_bps,
                        is_bytes: true,
                    },
                ]
            })
            .collect();
        let active = links.iter().filter(|l| l.is_active).count() as u32;
        let meta = NvLinkThroughputMeta {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            active_links: active,
            links: links.clone(),
        };
        let port = PortThroughput {
            dev_name: dev_name.to_string(),
            port: active,
            link_gbps: Some(50.0),
            tx_gbps: 0.0,
            rx_gbps: 0.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates,
            port_label: Some(format!("{}/{}", active, links.len())),
            nvlink: Some(meta.clone()),
            #[cfg(feature = "xgmi")]
            xgmi: None,
        };
        (port, meta)
    }

    fn join_lines(lines: &[Line<'static>]) -> String {
        let mut s = String::new();
        for line in lines {
            s.push_str(&line.to_string());
            s.push('\n');
        }
        s
    }

    #[test]
    fn header_contains_device_and_active_count() {
        let links = vec![
            make_link(
                0,
                true,
                RemoteDeviceType::Switch,
                Some("0000:01:00.0"),
                None,
                None,
                None,
                None,
                None,
                Some(3),
            ),
            make_link(
                1,
                true,
                RemoteDeviceType::Switch,
                Some("0000:01:00.0"),
                None,
                None,
                None,
                None,
                None,
                Some(3),
            ),
            make_link(
                2,
                false,
                RemoteDeviceType::Unknown,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        ];
        let (port, meta) = make_port("nvidia0", links, 0.0, 0.0);
        let tc = Theme::Default.colors();
        let lines = build_nvlink_detail_lines(&port, &meta, None, &tc, false, 0);
        let header = lines[0].to_string();
        assert!(
            header.contains("Device: nvidia0"),
            "header missing device label: {header:?}"
        );
        assert!(
            header.contains("[NVLink]"),
            "header missing NVLink tag: {header:?}"
        );
        assert!(
            header.contains("2/3 active"),
            "header missing active/total count: {header:?}"
        );
        // No avg label when show_avg=false.
        assert!(
            !header.contains("[avg"),
            "avg label should be hidden: {header:?}"
        );
    }

    #[test]
    fn inactive_link_renders_disabled_and_dash_remote() {
        let links = vec![make_link(
            0,
            false,
            RemoteDeviceType::Unknown,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )];
        let (port, meta) = make_port("nvidia0", links, 0.0, 0.0);
        let tc = Theme::Default.colors();
        let lines = build_nvlink_detail_lines(&port, &meta, None, &tc, false, 0);
        let blob = join_lines(&lines);
        assert!(blob.contains("disabled"), "missing 'disabled': {blob}");
        // Remote label column for Unknown remote with no BDF should be "-".
        // The row layout puts the remote label at the end; check it ends with "  -".
        let lane_row = lines
            .iter()
            .find(|l| l.to_string().contains("disabled"))
            .expect("lane row with disabled");
        let s = lane_row.to_string();
        assert!(
            s.trim_end().ends_with('-'),
            "inactive row should end with '-': {s:?}"
        );
    }

    #[test]
    fn active_link_renders_remote_pci_bdf() {
        let links = vec![make_link(
            2,
            true,
            RemoteDeviceType::Switch,
            Some("0000:01:00.0"),
            None,
            None,
            None,
            None,
            None,
            Some(4),
        )];
        let (port, meta) = make_port("nvidia0", links, 0.0, 0.0);
        let tc = Theme::Default.colors();
        let lines = build_nvlink_detail_lines(&port, &meta, None, &tc, false, 0);
        let blob = join_lines(&lines);
        assert!(
            blob.contains("NVSwitch 0000:01:00.0"),
            "active switch link missing remote label: {blob}"
        );
        assert!(
            blob.contains("enabled"),
            "active link row should be 'enabled': {blob}"
        );
    }

    #[test]
    fn error_line_sums_counters_across_links() {
        let links = vec![
            make_link(
                0,
                true,
                RemoteDeviceType::Switch,
                Some("0000:01:00.0"),
                None,
                None,
                Some(3),
                Some(5),
                Some(2),
                Some(4),
            ),
            make_link(
                1,
                true,
                RemoteDeviceType::Switch,
                Some("0000:01:00.0"),
                None,
                None,
                Some(7),
                Some(11),
                Some(13),
                Some(4),
            ),
            make_link(
                2,
                false,
                RemoteDeviceType::Unknown,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        ];
        let (port, meta) = make_port("nvidia0", links, 0.0, 0.0);
        let tc = Theme::Default.colors();
        let lines = build_nvlink_detail_lines(&port, &meta, None, &tc, false, 0);
        let error_line = lines
            .iter()
            .find(|l| l.to_string().contains("Errors:"))
            .expect("Errors line")
            .to_string();
        // crc = 3 + 7 = 10 (link 2 contributes 0)
        assert!(
            error_line.contains("crc=10"),
            "crc sum wrong: {error_line:?}"
        );
        // replay = 5 + 11 = 16
        assert!(
            error_line.contains("replay=16"),
            "replay sum wrong: {error_line:?}"
        );
        // recovery = 2 + 13 = 15
        assert!(
            error_line.contains("recovery=15"),
            "recovery sum wrong: {error_line:?}"
        );
    }

    #[test]
    fn rolling_average_label_appears_when_enabled() {
        let links = vec![make_link(
            0,
            true,
            RemoteDeviceType::Switch,
            Some("0000:01:00.0"),
            None,
            None,
            None,
            None,
            None,
            Some(4),
        )];
        let (port, meta) = make_port("nvidia0", links, 0.0, 0.0);
        let tc = Theme::Default.colors();
        let lines = build_nvlink_detail_lines(&port, &meta, None, &tc, true, 5);
        let header = lines[0].to_string();
        assert!(
            header.contains("[avg 5s]"),
            "rolling avg label missing: {header:?}"
        );
    }

    #[test]
    fn header_and_lanes_show_aggregate_rates() {
        // Two active links and one inactive link. The aggregate must appear
        // in the header (in Gbps) and be repeated identically in every lane
        // row (in MB/s) — NVML does not expose per-lane throughput.
        let links = vec![
            make_link(
                0,
                true,
                RemoteDeviceType::Switch,
                Some("0000:01:00.0"),
                None,
                None,
                None,
                None,
                None,
                Some(3),
            ),
            make_link(
                1,
                true,
                RemoteDeviceType::Gpu,
                Some("0000:02:00.0"),
                None,
                None,
                None,
                None,
                None,
                Some(3),
            ),
            make_link(
                2,
                false,
                RemoteDeviceType::Unknown,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        ];
        // Build a PortThroughput with non-zero aggregate rates. `make_port`
        // hardcodes the aggregate to 0.0, so construct it inline.
        let meta = NvLinkThroughputMeta {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            active_links: 2,
            links: links.clone(),
        };
        let port = PortThroughput {
            dev_name: "nvidia0".to_string(),
            port: 2,
            link_gbps: Some(100.0),
            tx_gbps: 1.0,
            rx_gbps: 1.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: Some("2/3".to_string()),
            nvlink: Some(meta.clone()),
            #[cfg(feature = "xgmi")]
            xgmi: None,
        };

        let tc = Theme::Default.colors();
        let lines = build_nvlink_detail_lines(&port, &meta, None, &tc, false, 0);

        // Header aggregate lines must contain the Gbps string.
        let tx_line = lines[1].to_string();
        let rx_line = lines[2].to_string();
        assert!(
            tx_line.contains("1.00 Gbps"),
            "TX header missing aggregate rate: {tx_line:?}"
        );
        assert!(
            rx_line.contains("1.00 Gbps"),
            "RX header missing aggregate rate: {rx_line:?}"
        );

        // Compute the expected MB/s value: gbps * 1000 / 8 = 125.0.
        let expected_mb_s = "125.0";

        // Muted note line must be present before the lane table.
        let blob = join_lines(&lines);
        assert!(
            blob.contains("Per-lane throughput is not available from NVML"),
            "missing muted note about per-lane throughput: {blob}"
        );

        // Each lane row must contain the aggregate MB/s value. Look up rows
        // by their link id and verify both TX and RX columns render 125.0.
        for link in &links {
            let lane_row = lines
                .iter()
                .find(|l| {
                    let s = l.to_string();
                    s.contains(&format!("{:>4}", link.link_id))
                })
                .unwrap_or_else(|| panic!("missing lane row for link {}", link.link_id))
                .to_string();
            // The MB/s value is rendered as `{:>6.1}` so it appears as
            // " 125.0" (leading space from the width spec).
            assert!(
                lane_row.contains(&format!("{}", expected_mb_s)),
                "lane {} row missing aggregate MB/s value {}: {:?}",
                link.link_id,
                expected_mb_s,
                lane_row
            );
        }

        // The MB/s value must appear in every lane row — that's three
        // occurrences total (one per link).
        let occurrences = lines
            .iter()
            .filter(|l| l.to_string().contains(expected_mb_s))
            .count();
        assert_eq!(
            occurrences, 3,
            "expected the aggregate MB/s value in 3 lane rows, got {occurrences}"
        );
    }

    #[test]
    fn lanes_show_per_lane_rates_when_present() {
        let links = vec![
            make_link(
                0,
                true,
                RemoteDeviceType::Switch,
                Some("0000:01:00.0"),
                Some(10_000_000), // 10 MB/s
                Some(20_000_000), // 20 MB/s
                None,
                None,
                None,
                Some(3),
            ),
            make_link(
                1,
                true,
                RemoteDeviceType::Gpu,
                Some("0000:02:00.0"),
                Some(30_000_000), // 30 MB/s
                Some(40_000_000), // 40 MB/s
                None,
                None,
                None,
                Some(3),
            ),
        ];
        let meta = NvLinkThroughputMeta {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            active_links: 2,
            links: links.clone(),
        };
        let port = PortThroughput {
            dev_name: "nvidia0".to_string(),
            port: 2,
            link_gbps: Some(100.0),
            tx_gbps: 1.0,
            rx_gbps: 1.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: Some("2/2".to_string()),
            nvlink: Some(meta.clone()),
            #[cfg(feature = "xgmi")]
            xgmi: None,
        };

        let tc = Theme::Default.colors();
        let lines = build_nvlink_detail_lines(&port, &meta, None, &tc, false, 0);

        // lane 0 row should contain "  10.0" and "  20.0"
        let lane0_row = lines
            .iter()
            .find(|l| l.to_string().starts_with("    0 "))
            .unwrap()
            .to_string();
        assert!(
            lane0_row.contains("  10.0"),
            "lane 0 missing TX: {lane0_row:?}"
        );
        assert!(
            lane0_row.contains("  20.0"),
            "lane 0 missing RX: {lane0_row:?}"
        );

        // lane 1 row should contain "  30.0" and "  40.0"
        let lane1_row = lines
            .iter()
            .find(|l| l.to_string().starts_with("    1 "))
            .unwrap()
            .to_string();
        assert!(
            lane1_row.contains("  30.0"),
            "lane 1 missing TX: {lane1_row:?}"
        );
        assert!(
            lane1_row.contains("  40.0"),
            "lane 1 missing RX: {lane1_row:?}"
        );
    }
}

#[cfg(all(test, feature = "xgmi"))]
mod xgmi_detail_tests {
    use super::*;
    use crate::tui::app::{PortThroughput, XgmiThroughputMeta};
    use crate::tui::theme::Theme;
    use crate::xgmi::XgmiLinkSnapshot;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn mk_link(id: u32, active: bool, tx_rate: u64, rx_rate: u64) -> XgmiLinkSnapshot {
        XgmiLinkSnapshot {
            link_id: id,
            is_active: active,
            speed_gbps: Some(512.0),
            bit_rate_gbps: Some(32.0),
            remote_pci_bdf: Some(format!("0000:{:02x}:00.0", 0x15 + id)),
            tx_bytes: Some(tx_rate),
            rx_bytes: Some(rx_rate),
        }
    }

    fn mk_row(links: Vec<XgmiLinkSnapshot>) -> (PortThroughput, XgmiThroughputMeta) {
        let active = links.iter().filter(|l| l.is_active).count() as u32;
        let meta = XgmiThroughputMeta {
            gpu_index: 0,
            gpu_name: "MI325X".to_string(),
            active_links: active,
            links,
        };
        let port = PortThroughput {
            dev_name: "amdgpu0".to_string(),
            port: active,
            link_gbps: Some(512.0 * active as f64),
            tx_gbps: 1.0,
            rx_gbps: 2.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: vec![
                crate::tui::app::CounterRate {
                    name: "xgmi_wafl_ce".to_string(),
                    value: 7,
                    delta: 0,
                    rate: 0.0,
                    is_bytes: false,
                },
                crate::tui::app::CounterRate {
                    name: "xgmi_wafl_ue".to_string(),
                    value: 1,
                    delta: 0,
                    rate: 0.0,
                    is_bytes: false,
                },
            ],
            port_label: Some(format!("{}/{}", active, meta.links.len())),
            #[cfg(feature = "nvlink")]
            nvlink: None,
            xgmi: Some(meta.clone()),
        };
        (port, meta)
    }

    #[test]
    fn header_shows_device_and_active_links() {
        let tc = Theme::Default.colors();
        let (port, meta) = mk_row(vec![mk_link(0, true, 0, 0), mk_link(1, false, 0, 0)]);
        let lines = build_xgmi_detail_lines(&port, &meta, None, &tc, false, 0);
        let header = line_text(&lines[0]);
        assert!(header.contains("amdgpu0"), "header: {header:?}");
        assert!(header.contains("[XGMI]"), "header: {header:?}");
        assert!(header.contains("1/2 active"), "header: {header:?}");
    }

    #[test]
    fn per_link_rows_show_rate_and_remote() {
        let tc = Theme::Default.colors();
        // 2_000_000 B/s -> 2.0 MB/s
        let (port, meta) = mk_row(vec![mk_link(0, true, 2_000_000, 4_000_000)]);
        let lines = build_xgmi_detail_lines(&port, &meta, None, &tc, false, 0);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        let row = text
            .iter()
            .find(|l| l.contains("0000:15:00.0"))
            .expect("link row");
        assert!(row.contains("up"), "row: {row:?}");
        assert!(row.contains("2.0"), "row: {row:?}");
        assert!(row.contains("4.0"), "row: {row:?}");
    }

    #[test]
    fn inactive_link_rendered_down() {
        let tc = Theme::Default.colors();
        let (port, meta) = mk_row(vec![mk_link(0, false, 0, 0)]);
        let lines = build_xgmi_detail_lines(&port, &meta, None, &tc, false, 0);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|l| l.contains("down")), "lines: {text:?}");
    }

    #[test]
    fn errors_line_shows_wafl_counts() {
        let tc = Theme::Default.colors();
        let (port, meta) = mk_row(vec![mk_link(0, true, 0, 0)]);
        let lines = build_xgmi_detail_lines(&port, &meta, None, &tc, false, 0);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        let err = text
            .iter()
            .find(|l| l.contains("wafl_ce"))
            .expect("errors line");
        assert!(err.contains("wafl_ce=7"), "err: {err:?}");
        assert!(err.contains("wafl_ue=1"), "err: {err:?}");
    }
}
