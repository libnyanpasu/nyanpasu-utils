#[cfg(target_os = "macos")]
pub mod macos {
    pub fn get_default_network_hardware_port() -> std::io::Result<String> {
        let dir = tempfile::tempdir()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        let path = dir.path().join("default-network-hardware-port.sh");
        std::fs::write(
            &path,
            include_bytes!("./scripts/find-macos-default-device-port.sh"),
        )?;
        let output = std::process::Command::new("osascript")
            .arg("-e")
            .arg(format!(
                "do shell script \"bash {}\"",
                path.to_string_lossy()
            ))
            .output()?;
        Ok(String::from_utf8(output.stdout)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?
            .trim()
            .to_string())
    }

    pub fn set_dns(service_name: &str, dns: Option<&str>) -> std::io::Result<()> {
        let dir = tempfile::tempdir()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        let path = dir.path().join("set-macos-dns.sh");
        std::fs::write(
            &path,
            include_str!("./scripts/set-macos-dns.sh")
                .replace("$1", service_name)
                .replace("$2", dns.unwrap_or("Empty")),
        )?;
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(format!(
                "do shell script \"bash {}\"",
                path.to_string_lossy()
            ))
            .status()?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_get_default_network_hardware_port() {
            let result = get_default_network_hardware_port();
            println!("{:?}", result);
        }

        #[test]
        fn test_set_dns() {
            set_dns("Wi-Fi", Some("114.114.114.114")).unwrap();
            set_dns("Wi-Fi", None).unwrap();
        }
    }
}
