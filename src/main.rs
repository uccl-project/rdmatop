use std::io;

use rdmatop::{trace, tui};

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() {
        tui::run()
    } else if args == ["--help"] || args == ["-h"] {
        println!("{}", trace::HELP);
        Ok(())
    } else {
        trace::run(trace::parse(&args)?)
    }
}
