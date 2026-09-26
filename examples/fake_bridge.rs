//! Test double for the NDJSON bridge protocol. Reads request lines, writes
//! response lines; no model involved. Flags:
//!   --check              print an available envelope
//!   --schema-check       accept a JSON-object schema without "bogus" in it
//!   --hang-on <id>       sleep 60s when this request id arrives (timeout test)
//!   --die-after <n>      exit after n requests (respawn test)
//!   --bad-id <id>        respond with a rotated id (dropped-response test)
//!   --check-reason <r>   with --check, report unavailable for reason r
//!   --check-garbage      with --check, print non-JSON
//!   --check-crash        with --check, exit 134 with loader-style stderr
//!   --unavailable <r>    answer requests with modelUnavailable and reason r
//!   --unavailable-bare   answer requests with modelUnavailable, no reason
//! With no flag, defaults to serve mode (matching the real bridge).

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

fn flag_value(args: &[String], name: &str) -> Option<u64> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

fn flag_text<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn audit(path: Option<&Path>, event: &str) {
    if let Some(path) = path {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{event}").unwrap();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let audit_path = args
        .iter()
        .position(|arg| arg == "--audit")
        .and_then(|index| args.get(index + 1))
        .map(Path::new);
    audit(audit_path, "spawn");
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    if args.iter().any(|a| a == "--check") {
        let mut out = stdout.lock();
        if args.iter().any(|a| a == "--check-crash") {
            eprintln!("dyld: Symbol not found: _$s16FoundationModels");
            std::process::exit(134);
        }
        if args.iter().any(|a| a == "--check-garbage") {
            writeln!(out, "not json").unwrap();
            return;
        }
        if let Some(reason) = flag_text(&args, "--check-reason") {
            let envelope = if reason == "-" {
                json!({"available": false, "provider": "apple"})
            } else {
                json!({"available": false, "provider": "apple", "reason": reason})
            };
            writeln!(out, "{envelope}").unwrap();
            return;
        }
        writeln!(
            out,
            "{}",
            json!({"available": true, "provider": "apple", "onDevice": true})
        )
        .unwrap();
        return;
    }
    if args.iter().any(|a| a == "--schema-check") {
        let mut input = String::new();
        BufReader::new(stdin.lock())
            .read_to_string(&mut input)
            .unwrap();
        let bad = match serde_json::from_str::<Value>(&input) {
            Ok(v) => !v.is_object() || v.to_string().contains("bogus"),
            Err(_) => true,
        };
        if bad {
            let mut err = std::io::stderr().lock();
            writeln!(
                err,
                "{}",
                json!({"ok": false, "error": {"code": "unsupportedSchemaType"}})
            )
            .unwrap();
            std::process::exit(1);
        }
        let mut out = stdout.lock();
        writeln!(out, "{}", json!({"ok": true})).unwrap();
        return;
    }

    if args.iter().any(|arg| arg == "--exit-after-prefix") {
        let mut prefix = [0; 1];
        stdin.lock().read_exact(&mut prefix).unwrap();
        audit(audit_path, "prefix");
        return;
    }
    if args.iter().any(|arg| arg == "--stall-after-prefix") {
        let mut prefix = [0; 1];
        stdin.lock().read_exact(&mut prefix).unwrap();
        audit(audit_path, "prefix");
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    let oversize = args.iter().any(|arg| arg == "--oversize-response");
    let drop_response = args.iter().any(|arg| arg == "--drop-response");
    let unlink_self = args.iter().any(|arg| arg == "--unlink-self-after-admit");
    let hang_on = flag_value(&args, "--hang-on").unwrap_or(0);
    let die_after = flag_value(&args, "--die-after").unwrap_or(u64::MAX);
    let bad_id = flag_value(&args, "--bad-id").unwrap_or(0);
    let unavailable = flag_text(&args, "--unavailable");
    let unavailable_bare = args.iter().any(|arg| arg == "--unavailable-bare");

    let mut count = 0u64;
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        if line.is_empty() {
            continue;
        }
        count += 1;
        if count > die_after {
            std::process::exit(1);
        }
        let req: Value = serde_json::from_str(&line).unwrap();
        audit(audit_path, "admit");
        if unlink_self {
            std::fs::remove_file(std::env::current_exe().unwrap()).unwrap();
        }
        if drop_response || unlink_self {
            return;
        }
        if oversize {
            // No newline: the client must stop at its byte cap, without waiting
            // for this stream to end or allocating the entire remote output.
            let bytes = [b'x'; 8192];
            for _ in 0..1024 {
                if out.write_all(&bytes).is_err() {
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(60));
            return;
        }
        let id = req["id"].as_u64().unwrap();
        if id == hang_on {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        let reported = if id == bad_id { id + 999 } else { id };
        let resp = if let Some(reason) = unavailable {
            json!({"id": reported, "ok": false, "error": {"code": "modelUnavailable", "reason": reason}})
        } else if unavailable_bare {
            json!({"id": reported, "ok": false, "error": {"code": "modelUnavailable"}})
        } else if req["prompt"].as_str().unwrap_or("") == "fail" {
            json!({"id": reported, "ok": false, "error": {"code": "generationFailed"}})
        } else {
            json!({"id": reported, "ok": true, "value": {"echo": req["prompt"]}})
        };
        writeln!(out, "{resp}").unwrap();
        out.flush().unwrap();
    }
}
