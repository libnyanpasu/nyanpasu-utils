use std::io::{Error as IoError, ErrorKind as IoErrorKind, Result as IoResult};
use tokio::process::Command;

pub async fn get_current_user_sid() -> IoResult<String> {
    let output = Command::new("cmd")
        .args(&["/C", "wmic useraccount where name='%username%' get sid"])
        .output()
        .await
        .map_err(|e| format!("Failed to execute command: {}", e))?;

    if !output.status.success() {
        return Err(IoError::new(IoErrorKind::Other, "Command failed"));
    }

    let output_str = str::from_utf8(&output.stdout)
        .map_err(|e| format!("Failed to convert output to string: {}", e))?;

    let lines: Vec<&str> = output_str.lines().collect();
    if lines.len() < 2 {
        return Err("Unexpected output format".to_string());
    }

    let sid = lines[1].trim().to_string();
    Ok(sid)
}