//! Version selection shared by configuration, archive decoding and the VM.
use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum EngineVersion {
    #[serde(rename = "椎名里緒 v2.36")]
    V2_36,
    #[default]
    #[serde(rename = "椎名里緒 v2.47")]
    V2_47,
}

impl EngineVersion {
    pub fn from_config(section: &str) -> Option<Self> {
        match section {
            "椎名里緒 v2.36" => Some(Self::V2_36),
            "椎名里緒 v2.47" => Some(Self::V2_47),
            _ => None,
        }
    }

    pub fn from_scheme(version: i128) -> Option<Self> {
        match version {
            2360 => Some(Self::V2_36),
            2470 => Some(Self::V2_47),
            _ => None,
        }
    }

    /// Values returned by SCN opcode 03c0 in the original executables.
    pub fn program_info(self) -> [u32; 2] {
        match self {
            Self::V2_36 => [236, 20050900],
            Self::V2_47 => [247, 20090401],
        }
    }
}
