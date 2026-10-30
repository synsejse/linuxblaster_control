fn main() -> linuxblaster_control::protocol::Result<()> {
    let mut device =
        linuxblaster_control::open(std::env::args().nth(1).as_deref())?;
    println!("{}", serde_json::to_string_pretty(&device.state()?)?);
    Ok(())
}
