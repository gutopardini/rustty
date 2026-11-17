fn main() {
    if let Err(err) = rustty::app::run() {
        eprintln!("rustty: {err:#}");
        std::process::exit(1);
    }
}
