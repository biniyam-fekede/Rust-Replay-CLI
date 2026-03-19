use crate::models::AggregatedStats;

pub fn print_summary(stats: &AggregatedStats, interrupted: bool) {
    if interrupted {
        println!("\n[INTERRUPTED] Partial run summary:");
    } else {
        println!("\nRun complete — Summary:");
    }
    println!("─────────────────────────────────────────");
    println!("  Lines read:        {}", stats.total_lines);
    println!("  Parse errors:      {}", stats.parse_errors);
    println!("  Filtered:          {}", stats.filtered_count);
    println!("  Attempted:         {}", stats.attempted);
    println!("  Succeeded (2xx):   {}", stats.succeeded);
    println!("  Failed:            {}", stats.failed);
    println!("  Elapsed:           {:.2}s", stats.elapsed_secs);
    println!("  Throughput:        {:.1} req/s", stats.throughput_rps);
    println!("─────────────────────────────────────────");
    println!("  Latency (µs):");
    println!("    p50:  {}", stats.p50_us);
    println!("    p95:  {}", stats.p95_us);
    println!("    p99:  {}", stats.p99_us);

    if !stats.status_distribution.is_empty() {
        println!("─────────────────────────────────────────");
        println!("  Status distribution:");
        let mut statuses: Vec<(&u16, &u64)> = stats.status_distribution.iter().collect();
        statuses.sort_by_key(|(s, _)| *s);
        for (status, count) in statuses {
            println!("    {}: {}", status, count);
        }
    }

    if !stats.error_categories.is_empty() {
        println!("─────────────────────────────────────────");
        println!("  Error categories:");
        for (cat, count) in &stats.error_categories {
            println!("    {}: {}", cat, count);
        }
    }
    println!("─────────────────────────────────────────");
}

pub fn write_stats_json(stats: &AggregatedStats, path: &std::path::Path) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(stats)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, json)
}
