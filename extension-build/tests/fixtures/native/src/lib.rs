//! Application-native functions: no ORM binding or host dependency.
pub fn trim(value: &str) -> Result<String, String> { Ok(value.trim().to_owned()) }
pub fn username(value: &str) -> Result<(), String> {
    if value.is_empty() || !value.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        Err("username must contain only letters, numbers, and underscores".into())
    } else { Ok(()) }
}
pub fn display(value: &str) -> Result<String, String> {
    if value == "unrequested" { return Err("this row must not be computed unless its result is requested".into()); }
    Ok(format!("Hello, {value}!"))
}
pub fn record(values: &[&str]) -> Result<(), String> {
    if values[0] == "admin" { Err("reserved record username".into()) } else { Ok(()) }
}
