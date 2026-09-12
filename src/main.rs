use eventstore::record;

fn main() {
    let payload = b"Hello, world";
    let length = match u32::try_from(payload.len()) {
        Ok(n) => n,
        Err(_) => 2,
    };

    println!("{}", record::checksum(&length.to_le_bytes(), payload));
}
