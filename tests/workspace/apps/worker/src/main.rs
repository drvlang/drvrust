fn main() {
    println!("worker {}", shared::message());
}
#[test]
fn message() {
    assert_eq!(shared::message(), "drvlang");
}
