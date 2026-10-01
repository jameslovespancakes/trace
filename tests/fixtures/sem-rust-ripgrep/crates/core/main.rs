mod hiargs;

fn main() {
    let args = hiargs::HiArgs::default();
    let builder = args.walk_builder();
    let _ = builder;
    grep_printer::run_json();
}
