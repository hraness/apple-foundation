//! Test double for the NDJSON bridge protocol. Reads request lines, writes
//! response lines; no model involved. Flags:
//!   --check              print an available envelope
//!   --schema-check       accept a JSON-object schema without "bogus" in it
//!   --hang-on <id>       sleep 60s when this request id arrives (timeout test)
//!   --die-after <n>      exit after n requests (respawn test)
//!   --bad-id <id>        respond with a rotated id (dropped-response test)
//! With no flag, defaults to serve mode (matching the real bridge).

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};

fn flag_value(args: &[String], name: &str) -> Option<u64> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    if args.iter().any(|a| a == "--check") {
        let mut out = stdout.lock();
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

    let hang_on = flag_value(&args, "--hang-on").unwrap_or(0);
    let die_after = flag_value(&args, "--die-after").unwrap_or(u64::MAX);
    let bad_id = flag_value(&args, "--bad-id").unwrap_or(0);

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
        let id = req["id"].as_u64().unwrap();
        if id == hang_on {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        let reported = if id == bad_id { id + 999 } else { id };
        let resp = if req["prompt"].as_str().unwrap_or("") == "fail" {
            json!({"id": reported, "ok": false, "error": {"code": "generationFailed"}})
        } else {
            json!({"id": reported, "ok": true, "value": {"echo": req["prompt"]}})
        };
        writeln!(out, "{resp}").unwrap();
        out.flush().unwrap();
    }
}
