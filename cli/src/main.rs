//! SlimDB command-line interface.
//!
//! ```text
//! slimdb <file.sdb> get <path>
//! slimdb <file.sdb> set <path> <json>
//! slimdb <file.sdb> delete <path>
//! slimdb <file.sdb> exists <path>
//! slimdb <file.sdb> keys
//! slimdb <file.sdb> stats
//! slimdb <file.sdb> compact
//! slimdb <file.sdb> clear
//! slimdb <file.sdb> backup <dest.sdb>
//! slimdb <file.sdb> restore <src.sdb>
//! ```
//!
//! Durability mode: `SLIMDB_DURABILITY=safe|balanced|fast` (default `safe`).

mod json;

use std::fmt;

use slimdb_core::{Db, DurabilityMode, SlimDbError};

/// CLI-level errors: either an engine error or a usage/parse error.
enum CliError {
    Usage(String),
    Slim(SlimDbError),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Usage(msg) => write!(f, "{msg}"),
            CliError::Slim(e) => write!(f, "{e}"),
        }
    }
}

impl From<SlimDbError> for CliError {
    fn from(e: SlimDbError) -> Self {
        CliError::Slim(e)
    }
}

impl From<String> for CliError {
    fn from(msg: String) -> Self {
        CliError::Usage(msg)
    }
}

type CmdResult = Result<(), CliError>;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || matches!(args[1].as_str(), "-h" | "--help") {
        print_usage();
        std::process::exit(if args.len() < 2 { 2 } else { 0 });
    }

    let db_path = args[1].as_str();
    let cmd = args.get(2).map(String::as_str).unwrap_or("help");

    let result = match cmd {
        "get" => cmd_get(db_path, &args),
        "set" => cmd_set(db_path, &args),
        "delete" => cmd_delete(db_path, &args),
        "exists" | "has" => cmd_exists(db_path, &args),
        "keys" => cmd_keys(db_path, &args),
        "stats" => cmd_stats(db_path, &args),
        "compact" => cmd_compact(db_path, &args),
        "clear" => cmd_clear(db_path, &args),
        "backup" => cmd_backup(db_path, &args),
        "restore" => cmd_restore(db_path, &args),
        other => {
            eprintln!("unknown command: {other}");
            print_usage();
            std::process::exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn mode_from_env() -> DurabilityMode {
    match std::env::var("SLIMDB_DURABILITY").as_deref() {
        Ok("balanced") => DurabilityMode::Balanced,
        Ok("fast") => DurabilityMode::Fast,
        _ => DurabilityMode::Safe,
    }
}

fn open(path: &str) -> Result<Db, SlimDbError> {
    Db::open_with_mode(path, mode_from_env())
}

fn require<'a>(args: &'a [String], n: usize, what: &str) -> Result<&'a str, String> {
    args.get(n)
        .map(String::as_str)
        .ok_or_else(|| format!("missing argument: {what}"))
}

fn cmd_get(db_path: &str, args: &[String]) -> CmdResult {
    let path = require(args, 3, "<path>")?;
    let db = open(db_path)?;
    if let Some(v) = db.get(path)? {
        println!("{}", json::stringify_pretty(&v));
    }
    Ok(())
}

fn cmd_set(db_path: &str, args: &[String]) -> CmdResult {
    let path = require(args, 3, "<path>")?;
    let json_text = require(args, 4, "<json>")?;
    let value = json::parse(json_text).map_err(CliError::Usage)?;
    let mut db = open(db_path)?;
    db.set(path, value)?;
    db.close()?;
    println!("OK");
    Ok(())
}

fn cmd_delete(db_path: &str, args: &[String]) -> CmdResult {
    let path = require(args, 3, "<path>")?;
    let mut db = open(db_path)?;
    let removed = db.delete(path)?;
    db.close()?;
    println!("{removed}");
    Ok(())
}

fn cmd_exists(db_path: &str, args: &[String]) -> CmdResult {
    let path = require(args, 3, "<path>")?;
    let db = open(db_path)?;
    println!("{}", db.exists(path)?);
    Ok(())
}

fn cmd_keys(db_path: &str, _args: &[String]) -> CmdResult {
    let db = open(db_path)?;
    for key in db.keys() {
        println!("{key}");
    }
    Ok(())
}

fn cmd_stats(db_path: &str, _args: &[String]) -> CmdResult {
    let db = open(db_path)?;
    let s = db.stats()?;
    println!("path:            {}", s.path);
    println!("keys:            {}", s.keys);
    println!("data_file_size:  {}", s.data_file_size);
    println!("wal_file_size:   {}", s.wal_file_size);
    println!("sequence:        {}", s.sequence);
    println!("durability:      {}", s.durability.name());
    Ok(())
}

fn cmd_compact(db_path: &str, _args: &[String]) -> CmdResult {
    let mut db = open(db_path)?;
    db.compact()?;
    println!("OK");
    Ok(())
}

fn cmd_clear(db_path: &str, _args: &[String]) -> CmdResult {
    let mut db = open(db_path)?;
    db.clear()?;
    db.close()?;
    println!("OK");
    Ok(())
}

fn cmd_backup(db_path: &str, args: &[String]) -> CmdResult {
    let dest = require(args, 3, "<dest.sdb>")?;
    let mut db = open(db_path)?;
    db.backup(dest)?;
    println!("OK");
    Ok(())
}

fn cmd_restore(db_path: &str, args: &[String]) -> CmdResult {
    let src = require(args, 3, "<src.sdb>")?;
    let mut db = open(db_path)?;
    db.restore(src)?;
    db.close()?;
    println!("OK");
    Ok(())
}

fn print_usage() {
    println!(
        "SlimDB — the tiny embedded database for local apps.\n\
         \n\
         Usage: slimdb <file.sdb> <command> [args...]\n\
         \n\
         Commands:\n\
         \x20 get <path>          print the value at path as JSON\n\
         \x20 set <path> <json>   set the value at path\n\
         \x20 delete <path>       delete the value at path\n\
         \x20 exists <path>       print true/false\n\
         \x20 keys                list top-level keys\n\
         \x20 stats               print database statistics\n\
         \x20 compact             compact the WAL into the snapshot\n\
         \x20 clear               remove all keys\n\
         \x20 backup <dest.sdb>   write a standalone snapshot\n\
         \x20 restore <src.sdb>   replace contents with a backup\n\
         \n\
         Environment:\n\
         \x20 SLIMDB_DURABILITY=safe|balanced|fast   (default safe)\n\
         \n\
         Examples:\n\
         \x20 slimdb game.sdb set player.level 42\n\
         \x20 slimdb game.sdb set player '{{\"name\": \"Aurel\"}}'\n\
         \x20 slimdb game.sdb get player.level"
    );
}
