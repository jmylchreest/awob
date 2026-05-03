fn main() {
    let path = std::env::args().nth(1).expect("usage: kdl_check <file>");
    let s = std::fs::read_to_string(&path).unwrap();
    match s.parse::<kdl::KdlDocument>() {
        Ok(d) => println!("OK: {} nodes", d.nodes().len()),
        Err(e) => {
            println!("ERR: {e}");
            for d in &e.diagnostics {
                println!("  - {d:?}");
            }
        }
    }
}
