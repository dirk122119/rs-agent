fn main() {
    let prompt: String = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if prompt.is_empty() {
        eprintln!("usage: rs-agent <prompt>");
        std::process::exit(1);
    }
    println!("你說：{prompt}");
}
