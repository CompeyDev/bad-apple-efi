#[derive(Debug, Clone)]
pub struct ArchiveReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> ArchiveReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    pub fn next_file(&mut self) -> Option<(&'a str, &'a [u8])> {
        if self.position >= self.data.len() {
            return None;
        }

        let name_len = self.data[self.position] as usize;
        self.position += 1;

        let name_bytes = &self.data[self.position..self.position + name_len];
        let name = core::str::from_utf8(name_bytes).ok()?;
        self.position += name_len;

        let data_len = u32::from_le_bytes([
            self.data[self.position],
            self.data[self.position + 1],
            self.data[self.position + 2],
            self.data[self.position + 3],
        ]) as usize;
        self.position += 4;

        let data = &self.data[self.position..self.position + data_len];
        self.position += data_len;

        Some((name, data))
    }
}
