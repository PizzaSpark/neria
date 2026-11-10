//! Check for a new Neria and newer tool builds.
//! `NERIA_HOME=target/debug cargo run -p neria-core --example updates [-- --any-age] [--update <tool name start>]`
//! `--update` really re-downloads that tool into NERIA_HOME/bin (point it at a scratch copy).

use neria_core::update;

fn main() {
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
