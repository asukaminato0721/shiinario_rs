//! Version selection shared by configuration, archive decoding and the VM.
use serde::{Serialize, Serializer};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EngineVersion {
    V2_36,
    #[default]
    V2_47,
    V2_48,
    V2_49,
    /// Recognized engine version that uses the available parsers on a trial basis.
    Unverified(u16),
}

impl EngineVersion {
    pub fn from_config(section: &str) -> Option<Self> {
        let minor = section.strip_prefix("椎名里緒 v2.")?;
        if minor.len() != 2 || !minor.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Self::from_scheme(2000 + minor.parse::<i128>().ok()? * 10)
    }

    pub fn from_scheme(version: i128) -> Option<Self> {
        match version {
            2360 => Some(Self::V2_36),
            2470 => Some(Self::V2_47),
            2480 => Some(Self::V2_48),
            2490 => Some(Self::V2_49),
            2000..=2990 if version % 10 == 0 => Some(Self::Unverified((version / 10) as u16)),
            _ => None,
        }
    }

    pub fn number(self) -> u16 {
        match self {
            Self::V2_36 => 236,
            Self::V2_47 => 247,
            Self::V2_48 => 248,
            Self::V2_49 => 249,
            Self::Unverified(number) => number,
        }
    }

    /// Add this diagnostic only after a parser fails. Recognition is not a support check.
    pub fn failure_context(self) -> Option<String> {
        matches!(self, Self::Unverified(_)).then(|| {
            format!(
                "parsing failed for unverified ShiinaRio v{}.{:02}. Version compatibility is not established",
                self.number() / 100,
                self.number() % 100
            )
        })
    }

    /// Values returned by SCN opcode 03c0 in the original executables.
    pub fn program_info(self) -> [u32; 2] {
        match self {
            Self::V2_36 => [236, 20050900],
            Self::V2_47 => [247, 20090401],
            Self::V2_48 => [248, 20101101],
            Self::V2_49 => [249, 20110301],
            // Preserve the reported version. The original build date is unknown.
            Self::Unverified(number) => [number as u32, 0],
        }
    }
}

impl Serialize for EngineVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!(
            "椎名里緒 v{}.{:02}",
            self.number() / 100,
            self.number() % 100
        ))
    }
}
