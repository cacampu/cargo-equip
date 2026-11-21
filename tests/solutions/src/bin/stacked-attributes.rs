use expansion_fixture::{expand_number, preserve_first, preserve_second};

#[preserve_first]
#[preserve_second]
fn main() {
    let number = expand_number!();
    println!("{}", number + ORIGINAL);
}

const ORIGINAL: u32 = expand_number!();
