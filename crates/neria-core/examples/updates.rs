//! Check for a new Neria and newer tool builds.
//! `NERIA_HOME=target/debug cargo run -p neria-core --example updates [-- --any-age] [--update <tool name start>]`
//! `--update` really re-downloads that tool into NERIA_HOME/bin (point it at a scratch copy).
//! `--ytdlp`: yt-dlp's onedir update by itself (which yt-dlp runs, the newest release, then
//! fetch / check / swap / tidy); `NERIA_YTDLP_TAG=<tag>`
//! pretends that release is the newest (to install an older one and update from it).

use neria_core::update;

fn main() {
    if std::env::args().any(|a| a == "--ytdlp") {
        use neria_core::ytdlp;
        println!("runs: {} ({:?})", ytdlp::exe().display(), ytdlp::works(&ytdlp::exe()));
        match ytdlp::latest() {
            Ok(l) => {
                println!("newest: {} ({} days old), zip sha256 {:?}", l.version, l.days_old().unwrap_or(0), l.sum())
            }
            Err(e) => println!("newest: {e:#}"),
        }
        let t = std::time::Instant::now();
        let r = ytdlp::update(|d, total| eprint!("\r{d} / {total:?}            "));
        println!("\n{r:?} in {:.1} s", t.elapsed().as_secs_f64());
        println!("runs: {} ({:?})", ytdlp::exe().display(), ytdlp::works(&ytdlp::exe()));
        println!("one-file left: {}", ytdlp::onefile().is_file());
        let mut left: Vec<String> = std::fs::read_dir(ytdlp::dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        println!("bin/yt-dlp: {left:?}");
        return;
    }
    match update::check_neria() {
        Ok(Some(r)) => println!("Neria {} is out: {}\n{}", r.version, r.zip_url, update::plain_notes(&r.notes)),
        Ok(None) => println!("Neria is up to date"),
        Err(e) => println!("Neria check failed: {e:#}"),
    }
    let args: Vec<String> = std::env::args().collect();
    let days = if args.iter().any(|a| a == "--any-age") { 0 } else { update::TOOL_MAX_AGE_DAYS };
    match update::check_tools_older_than(days) {
        Ok(list) if list.is_empty() => println!("Tools: nothing to update (limit {days} days)"),
        Ok(list) => list.iter().for_each(|t| println!("Tool: {} is {} days behind", t.label, t.behind_days)),
        Err(e) => println!("Tool check failed: {e:#}"),
    }
    if let Some(i) = args.iter().position(|a| a == "--update") {
        let prefix = args.get(i + 1).map(String::as_str).unwrap_or("");
        let labels: Vec<&str> =
            neria_core::tools::TOOLS.iter().map(|t| t.label).filter(|l| l.starts_with(prefix)).collect();
        println!("Updating {labels:?}");
        match update::update_tools(&labels, |p| eprint!("\r{}            ", p.text())) {
            Ok(()) => println!("\nUpdated"),
            Err(e) => println!("\nUpdate failed: {e:#}"),
        }
    }
}
