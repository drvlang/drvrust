fn main() {
    println!("api {}", shared::message());
}
#[test]
fn message() {
    assert_eq!(shared::message(), "drvlang");
}
