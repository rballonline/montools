//! Command-line companion to the tray app. Shares its config file.

use anyhow::{anyhow, bail, Result};
use montools_lib::config::{self, Config};
use montools_lib::inputs::parse_input;
use montools_lib::kvm::KvmState;
use montools_lib::monitors::Monitors;

const USAGE: &str = "\
montools-cli — switch monitor inputs and KVM USB mapping over DDC/CI

USAGE:
  montools-cli [--monitor <key>] <command>

COMMANDS:
  list [--all]                 KVM monitors (or all with --all), inputs and state
  status                       Current input of the selected monitor
  switch <input>               Switch input: hdmi1, hdmi2, dp, usbc, 0x11…
  switch toggle|this|other     Switch using the inputs saved in the config
  kvm show                     Which USB upstream each input uses
  kvm set <input> <port>       Map an input to a USB port: b1, b2, usbc
  config                       Show config path and contents
  config set this|other <input>
  config name this|other <name>  Friendly name for a computer (empty clears it)
";

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn print_kvm(cfg: &Config, k: &KvmState) {
    println!("  KVM mapping (raw 0x{:04X}):", k.raw);
    for s in &k.slots {
        let who = cfg.computer_name(s.input).map(|n| format!("  [{n}]")).unwrap_or_default();
        println!("    {:<6} → {:<7}{who}", s.input_name, s.port_name);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = Config::load();
    let mut monitor = cfg.monitor.clone();
    if let Some(i) = args.iter().position(|a| a == "--monitor" || a == "-m") {
        args.remove(i);
        if i >= args.len() {
            bail!("--monitor needs a value");
        }
        monitor = Some(args.remove(i));
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();

    match a.as_slice() {
        [] | ["help"] | ["-h"] | ["--help"] => print!("{USAGE}"),

        ["config"] => {
            println!("{}", config::path().display());
            println!("{}", serde_json::to_string_pretty(&cfg)?);
        }
        ["config", "set", which @ ("this" | "other"), input] => {
            let code = parse_input(input).ok_or_else(|| anyhow!("unknown input '{input}'"))?;
            if *which == "this" { cfg.this_input = Some(code) } else { cfg.other_input = Some(code) }
            cfg.save()?;
            println!("{which} PC = {}", cfg.input_label(code));
        }
        ["config", "name", which @ ("this" | "other"), name] => {
            let name = Some(name.trim().to_string()).filter(|n| !n.is_empty());
            if *which == "this" { cfg.this_name = name } else { cfg.other_name = name }
            cfg.save()?;
            println!("saved");
        }

        ["list"] | ["list", "--all"] => {
            let mut m = Monitors::enumerate();
            let selected = m.resolve(monitor.as_deref()).ok();
            let all = m.all_status();
            let show_all = a.len() == 2 || cfg.show_all_monitors || !all.iter().any(|s| s.kvm.is_some());
            let hidden = all.iter().filter(|s| s.kvm.is_none()).count();
            for s in all.into_iter().filter(|s| show_all || s.kvm.is_some()) {
                let mark = if Some(&s.key) == selected.as_ref() { "*" } else { " " };
                println!("{mark} {}", s.key);
                println!(
                    "  inputs: {}",
                    s.inputs.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")
                );
                if let Some(i) = s.current_input {
                    println!("  current: {}", cfg.input_label(i));
                }
                if let Some(k) = &s.kvm {
                    print_kvm(&cfg, k);
                }
            }
            if !show_all && hidden > 0 {
                println!("({hidden} monitor(s) without a known KVM hidden; use --all)");
            }
        }

        ["status"] => {
            let mut m = Monitors::enumerate();
            let key = m.resolve(monitor.as_deref())?;
            println!("{}: {}", key, cfg.input_label(m.current_input(&key)?));
        }

        ["switch", target] => {
            let mut m = Monitors::enumerate();
            let key = m.resolve(monitor.as_deref())?;
            let code = match *target {
                "this" => cfg.this_input.ok_or_else(|| anyhow!("run: config set this <input>"))?,
                "other" => cfg.other_input.ok_or_else(|| anyhow!("run: config set other <input>"))?,
                "toggle" => {
                    let cur = m.current_input(&key)?;
                    cfg.toggle_target(cur)
                        .ok_or_else(|| anyhow!("set both 'this' and 'other' inputs first (config set …)"))?
                }
                t => parse_input(t).ok_or_else(|| anyhow!("unknown input '{t}'"))?,
            };
            m.set_input(&key, code)?;
            println!("{key} → {}", cfg.input_label(code));
        }

        ["kvm", "show"] => {
            let mut m = Monitors::enumerate();
            let key = m.resolve(monitor.as_deref())?;
            m.kvm_profile(&key)?;
            println!("{key}");
            print_kvm(&cfg, m.status(&key)?.kvm.as_ref().unwrap());
        }
        ["kvm", "set", input, port] => {
            let mut m = Monitors::enumerate();
            let key = m.resolve(monitor.as_deref())?;
            let profile = m.kvm_profile(&key)?;
            let code = parse_input(input).ok_or_else(|| anyhow!("unknown input '{input}'"))?;
            let p = profile
                .parse_port(port)
                .ok_or_else(|| anyhow!("unknown USB port '{port}' (use b1, b2 or usbc)"))?;
            let k = m.set_kvm_port(&key, code, p)?;
            println!("{key}");
            print_kvm(&cfg, &k);
        }

        _ => bail!("unrecognized command\n\n{USAGE}"),
    }
    Ok(())
}
