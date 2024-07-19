#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{borrow::Cow, ffi::OsStr, path::Path};

#[cfg(feature = "serde")]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ClashCoreType {
    #[serde(rename = "mihomo")]
    Mihomo,
    #[serde(rename = "mihomo-alpha")]
    MihomoAlpha,
    #[serde(rename = "clash-rs")]
    ClashRust,
    #[serde(rename = "clash")]
    ClashPremium,
}

#[cfg(not(feature = "serde"))]
#[derive(Debug, Clone, Copy)]
pub enum ClashCoreType {
    Mihomo,
    MihomoAlpha,
    ClashRust,
    ClashPremium,
}

impl ClashCoreType {
    pub(super) fn get_run_args<'a, P: Into<Cow<'a, Path>>>(
        &self,
        app_dir: P,
        config_path: P,
    ) -> Vec<Cow<'a, OsStr>> {
        let app_dir: Cow<'a, Path> = app_dir.into();
        let config_path: Cow<'a, Path> = config_path.into();
        let app_dir: Cow<'a, OsStr> = Cow::Owned(app_dir.as_ref().as_os_str().to_owned());
        let config_path: Cow<'a, OsStr> = Cow::Owned(config_path.as_ref().as_os_str().to_owned());
        match self {
            ClashCoreType::Mihomo | ClashCoreType::MihomoAlpha => vec![
                Cow::Borrowed(OsStr::new("-m")),
                Cow::Borrowed(OsStr::new("-d")),
                app_dir,
                Cow::Borrowed(OsStr::new("-f")),
                config_path,
            ],
            ClashCoreType::ClashRust => {
                vec![
                    Cow::Borrowed(OsStr::new("-d")),
                    app_dir,
                    Cow::Borrowed(OsStr::new("-c")),
                    config_path,
                ]
            }
            ClashCoreType::ClashPremium => {
                vec![
                    Cow::Borrowed(OsStr::new("-d")),
                    app_dir,
                    Cow::Borrowed(OsStr::new("-f")),
                    config_path,
                ]
            }
        }
    }
}

#[cfg(feature = "serde")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreType {
    #[serde(rename = "clash")]
    Clash(ClashCoreType),
    #[serde(rename = "singbox")]
    SingBox, // Maybe we would support this in the 2.x?
}

#[cfg(not(feature = "serde"))]
#[derive(Debug, Clone)]
pub enum CoreType {
    Clash(ClashCoreType),
    SingBox, // Maybe we would support this in the 2.x?
}

/// TODO: give a idea to show the meta tags of a core
/// such as the build info, gccgo, llvm-go, amdv3, amdv4 etc.
impl CoreType {
    pub fn get_executable_name(&self) -> &'static str {
        match self {
            CoreType::Clash(ClashCoreType::Mihomo) => {
                constcat::concat!("mihomo", std::env::consts::EXE_SUFFIX)
            }
            CoreType::Clash(ClashCoreType::MihomoAlpha) => {
                constcat::concat!("mihomo-alpha", std::env::consts::EXE_SUFFIX)
            }
            CoreType::Clash(ClashCoreType::ClashRust) => {
                constcat::concat!("clash-rs", std::env::consts::EXE_SUFFIX)
            }
            CoreType::Clash(ClashCoreType::ClashPremium) => {
                constcat::concat!("clash", std::env::consts::EXE_SUFFIX)
            }
            CoreType::SingBox => {
                constcat::concat!("singbox", std::env::consts::EXE_SUFFIX)
            }
        }
    }
}

// TODO: impl downloadable core and core with different tags
pub struct CoreMetaData {
    downloaded: bool,
}

pub type CoresMetaMap = HashMap<CoreType, CoreMetaData>;

#[derive(Debug, Clone)]
pub struct TerminatedPayload {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

pub enum CommandEvent {
    Stdout(String),
    Stderr(String),
    Error(String),
    Terminated(TerminatedPayload),
    DelayCheckpointPass, // Custom event for a delay health check
}
