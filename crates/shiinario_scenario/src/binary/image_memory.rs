//! Shared image memory exposed through the platform interface.
use super::*;

impl BinaryVm {
    /// Map a raw image frame once. All later writes use the same shared region.
    pub fn respond_image_pixels(&mut self, memory: SharedMemory, offset: usize) -> Result<u32> {
        let Some((
            Event::Platform {
                request: PlatformRequest::ImagePixels { image, frame, .. },
                ..
            },
            destination,
        )) = &self.pending
        else {
            bail!("no pending image pixel request");
        };
        let key = (*image, *frame);
        let destination = destination.context("image pointer requires a destination")?;
        ensure!(offset < memory.len(), "image pixel offset exceeds memory");
        let existing = self
            .shared_regions
            .iter()
            .find(|(_, region)| region.same_region(&memory))
            .map(|(&base, _)| base);
        let base = if let Some(base) = existing {
            base
        } else {
            ensure!(
                self.shared_regions
                    .values()
                    .map(SharedMemory::len)
                    .sum::<usize>()
                    + memory.len()
                    <= 256 * 1024 * 1024,
                "mapped resources exceed 256 MiB"
            );
            let base = self.next_shared;
            let next = base
                .checked_add(((memory.len() as u32 + 4095) & !4095) + 4096)
                .context("shared memory address overflow")?;
            ensure!(next < 0xf000_0000, "shared memory address space exhausted");
            self.shared_regions.insert(base, memory);
            self.next_shared = next;
            base
        };
        let address = base
            .checked_add(offset as u32)
            .context("image pixel pointer overflow")?;
        self.write(destination, address)?;
        if let Some(old) = self
            .raw_image_regions
            .insert(key, base)
            .filter(|old| *old != base)
        {
            self.shared_regions.remove(&old);
        }
        self.pending = None;
        Ok(address)
    }
}
