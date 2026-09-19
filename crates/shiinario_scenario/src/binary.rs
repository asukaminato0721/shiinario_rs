//! Verified subset of the supplied v2.47 engine. See docs/SCN_RESEARCH.md.
use crate::{Event, Location};
use anyhow::{Context, Result, bail, ensure};

/// The original engine stores the full operand and treats every nonzero value as
/// swapped. Input bits 0, 1 and 2 represent the first three physical mouse buttons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseButtonMapping {
    pub value: u32,
}
impl MouseButtonMapping {
    pub fn map_buttons(self, physical: u8) -> u8 {
        let buttons = physical & 7;
        if self.value == 0 {
            buttons
        } else {
            ((buttons & 1) << 1) | ((buttons & 2) >> 1) | (buttons & 4)
        }
    }
}

pub struct BinaryVm {
    name: String,
    data: Vec<u8>,
    pc: usize,
    steps: usize,
    mouse_mapping: MouseButtonMapping,
    failure: Option<String>,
}
impl BinaryVm {
    pub fn new(name: impl Into<String>, data: Vec<u8>) -> Result<Self> {
        ensure!(data.len() <= 16 * 1024 * 1024, "scenario exceeds size cap");
        Ok(Self {
            name: name.into(),
            data,
            pc: 0,
            steps: 0,
            mouse_mapping: MouseButtonMapping::default(),
            failure: None,
        })
    }
    pub fn location(&self) -> Location {
        Location {
            scenario: self.name.clone(),
            offset: self.pc,
            line: None,
        }
    }
    pub fn mouse_mapping(&self) -> MouseButtonMapping {
        self.mouse_mapping
    }
    pub fn step(&mut self) -> Result<Event> {
        if let Some(error) = &self.failure {
            bail!("{error}");
        }
        match self.execute() {
            Ok(event) => Ok(event),
            Err(error) => {
                let message = format!(
                    "{}:{:#x}: {error:#}; executed_steps={}, mouse_button_mapping={:#x}, next_bytes={:02x?}",
                    self.name,
                    self.pc,
                    self.steps,
                    self.mouse_mapping.value,
                    &self.data[self.pc..self.data.len().min(self.pc + 24)]
                );
                self.failure = Some(message.clone());
                bail!("{message}");
            }
        }
    }
    fn execute(&mut self) -> Result<Event> {
        let location = self.location();
        let remaining = &self.data[self.pc..];
        let bytes = remaining.get(..2).context("truncated SCN opcode")?;
        let opcode = u16::from_le_bytes([bytes[0], bytes[1]]);
        match opcode {
            0x049d => {
                // Original: 0x426922 -> 0x41d7e0 -> operand reader 0x414e30.
                // Only the unflagged immediate form is implemented here. Other
                // forms require real variable banks/address semantics first.
                let tag = *remaining.get(2).context("0x049d: missing operand tag")?;
                ensure!(
                    tag == 0x04,
                    "0x049d: unsupported operand tag {tag:#04x} at {:#x}",
                    self.pc + 2
                );
                let bytes: [u8; 4] = remaining
                    .get(3..7)
                    .context("0x049d: truncated immediate operand")?
                    .try_into()?;
                self.mouse_mapping.value = u32::from_le_bytes(bytes);
                self.pc += 7;
                self.steps += 1;
                Ok(Event::MouseButtonMapping {
                    location,
                    value: self.mouse_mapping.value,
                })
            }
            _ => bail!("unsupported binary SCN opcode 0x{opcode:04x}"),
        }
    }
}

/// Execute verified instructions and stop at the first unresolved operation.
/// Reaching the end of the byte buffer is not an inferred engine exit opcode.
pub fn boot_binary(name: &str, data: &[u8]) -> Result<()> {
    let mut vm = BinaryVm::new(name, data.to_vec())?;
    loop {
        vm.step()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected masks come from executing the captured x86 dispatcher and mouse
    // mapper with synthetic inputs, not from the Rust implementation.
    #[test]
    fn matches_original_machine_code_probe() {
        for (value, masks) in [
            (0, [0, 1, 2, 3, 4, 5, 6, 7]),
            (1, [0, 2, 1, 3, 4, 6, 5, 7]),
            (2, [0, 2, 1, 3, 4, 6, 5, 7]),
            (u32::MAX, [0, 2, 1, 3, 4, 6, 5, 7]),
        ] {
            let mut code = vec![0x9d, 0x04, 0x04];
            code.extend(value.to_le_bytes());
            code.extend([0x0a, 0]);
            let mut vm = BinaryVm::new("synthetic.scn", code).unwrap();
            assert!(
                matches!(vm.step().unwrap(), Event::MouseButtonMapping { value: v, .. } if v == value)
            );
            assert_eq!(vm.location().offset, 7);
            assert_eq!(vm.mouse_mapping().value, value);
            for (physical, expected) in masks.into_iter().enumerate() {
                assert_eq!(vm.mouse_mapping().map_buttons(physical as u8), expected);
            }
            let error = vm.step().unwrap_err().to_string();
            assert!(error.contains("synthetic.scn:0x7"));
            assert!(error.contains("opcode 0x000a"));
            assert_eq!(error, vm.step().unwrap_err().to_string());
            assert_eq!(vm.location().offset, 7);
        }
    }

    #[test]
    fn truncated_or_unresolved_operands_do_not_change_state() {
        let full = [0x9d, 4, 4, 1, 0, 0, 0];
        for end in 0..full.len() {
            let mut vm = BinaryVm::new("short.scn", full[..end].to_vec()).unwrap();
            assert!(vm.step().is_err());
            assert_eq!(vm.location().offset, 0);
            assert_eq!(vm.mouse_mapping().value, 0);
        }
        for tag in [0x02, 0x44, 0x84, 0xff] {
            let mut vm = BinaryVm::new("operand.scn", vec![0x9d, 4, tag, 0, 0, 0, 0]).unwrap();
            assert!(
                vm.step()
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported operand tag")
            );
            assert_eq!(vm.location().offset, 0);
        }
    }

    #[test]
    fn subsequent_mapping_replaces_previous_value() {
        let mut vm = BinaryVm::new(
            "reset.scn",
            vec![0x9d, 4, 4, 1, 0, 0, 0, 0x9d, 4, 4, 0, 0, 0, 0],
        )
        .unwrap();
        vm.step().unwrap();
        assert_eq!(vm.mouse_mapping().map_buttons(1), 2);
        vm.step().unwrap();
        assert_eq!(vm.mouse_mapping().map_buttons(1), 1);
        assert_eq!(vm.location().offset, 14);
        assert!(
            vm.step()
                .unwrap_err()
                .to_string()
                .contains("truncated SCN opcode")
        );
    }
}
