use gqlrust::elaborate;
use gqlrust::parser;
use gqlrust::typing::checker::Typechecker;

fn main() {
    let q = std::env::args()
        .nth(1)
        .expect("usage: typecheck_demo \"<query>\"");
    let parsed = parser::parse_query(&q).expect("parse failed");
    let elaborated = elaborate::elaborate_query(parsed);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&elaborated);

    println!("ok       = {}", r.ok);
    println!("empty    = {}", r.empty);
    println!("path     = {:?}", r.path);
    println!("env keys = {:?}", r.env.keys());
    println!("errors   = {:?}", tc.errors);
    println!("warnings = {:?}", tc.warnings);
}
