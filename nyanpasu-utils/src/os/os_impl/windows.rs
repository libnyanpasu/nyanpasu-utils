use std::io::{Error as IoError, ErrorKind as IoErrorKind, Result as IoResult};
use tokio::process::Command;

pub async fn get_current_user_sid() -> IoResult<String> {
    let output = Command::new("cmd")
        .args(["/C", "wmic useraccount where name='%username%' get sid"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .await
        .map_err(|e| {
            IoError::new(
                IoErrorKind::Other,
                format!("Failed to execute command: {}", e),
            )
        })?;

    if !output.status.success() {
        return Err(IoError::new(IoErrorKind::Other, "Command failed"));
    }

    let output_str = String::from_utf8_lossy(&output.stdout);

    let lines: Vec<&str> = output_str.lines().collect();
    if lines.len() < 2 {
        return Err(IoError::new(IoErrorKind::Other, "Unexpected output format"));
    }

    let sid = lines[1].trim().to_string();
    Ok(sid)
}
