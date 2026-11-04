fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut device = soundblaster_x4::open(std::env::args().nth(1).as_deref())?;
    println!("{}", serde_json::to_string_pretty(&device.state()?)?);
    Ok(())
}
