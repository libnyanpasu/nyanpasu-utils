use std::io::{Error as IoError, ErrorKind as IoErrorKind, Result as IoResult};
use tokio::process::Command;

async fn execute_command(command: &str, args: &[&str]) -> IoResult<String> {
    let output = Command::new(command)
        .args(args)
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
        return Err(IoError::new(
            IoErrorKind::Other,
            format!("Command execution failed: '{} {}'", command, args.join(" ")),
        ));
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    Ok(output_str.trim().to_string())
}

pub async fn get_current_user_sid() -> IoResult<String> {
    let wmic_command = "cmd";
    let wmic_args = ["/C", "wmic useraccount where name='%username%' get sid"];
    let powershell_command = "powershell";
    let powershell_args = ["-Command", "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value"];
    let fallback_powershell_args = ["-Command", "Get-WmiObject Win32_UserAccount -Filter \"Name='$env:USERNAME'\" | Select-Object -ExpandProperty SID"];

    match execute_command(wmic_command, &wmic_args).await {
        Ok(output_str) => {
            let lines: Vec<&str> = output_str.lines().collect();
            if lines.len() < 2 {
                return Err(IoError::new(IoErrorKind::Other, "Unexpected output format"));
            }
            Ok(lines[1].trim().to_string())
        }
        Err(_) => {
            // Fallback to PowerShell if wmic fails
            match execute_command(powershell_command, &powershell_args).await {
                Ok(sid) => Ok(sid),
                Err(_) => execute_command(powershell_command, &fallback_powershell_args).await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Runtime;

    #[tokio::test]
    #[cfg(windows)]
    async fn test_get_current_user_sid() {
        match get_current_user_sid().await {
            Ok(sid) => {
                println!("[{}]", sid);
                assert!(!sid.is_empty(), "SID should not be empty");
            }
            Err(e) => {
                panic!("Failed to get current user SID: {}", e);
            }
        }
    }
}