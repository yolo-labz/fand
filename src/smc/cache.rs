const CACHE_SIZE: usize = 256;

pub struct KeyInfoCache {
    slots: [Option<(u32, u32)>; CACHE_SIZE],
}

impl KeyInfoCache {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [None; CACHE_SIZE],
        }
    }

    #[must_use]
    pub fn get(&self, fourcc: u32) -> Option<(u32, u32)> {
        let idx = (fourcc as usize) % CACHE_SIZE;
        self.slots[idx]
    }

    pub fn put(&mut self, fourcc: u32, data_size: u32, data_type: u32) {
        let idx = (fourcc as usize) % CACHE_SIZE;
        self.slots[idx] = Some((data_size, data_type));
    }

    pub fn invalidate(&mut self, fourcc: u32) {
        let idx = (fourcc as usize) % CACHE_SIZE;
        self.slots[idx] = None;
    }
}

impl Default for KeyInfoCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_empty_returns_none() {
        let cache = KeyInfoCache::new();
        assert_eq!(cache.get(0x46306D64), None); // F0Md
    }

    #[test]
    fn put_and_get() {
        let mut cache = KeyInfoCache::new();
        let fourcc = 0x46306D64; // F0Md
        cache.put(fourcc, 1, 0x75693820); // ui8
        assert_eq!(cache.get(fourcc), Some((1, 0x75693820)));
    }

    #[test]
    fn invalidate_clears() {
        let mut cache = KeyInfoCache::new();
        let fourcc = 0x46306D64;
        cache.put(fourcc, 1, 0x75693820);
        cache.invalidate(fourcc);
        assert_eq!(cache.get(fourcc), None);
    }

    #[test]
    fn collision_overwrites() {
        let mut cache = KeyInfoCache::new();
        let a = 0u32;
        let b = CACHE_SIZE as u32; // same slot
        cache.put(a, 1, 100);
        cache.put(b, 4, 200);
        assert_eq!(cache.get(b), Some((4, 200)));
        // 'a' was overwritten
        assert_eq!(cache.get(a), Some((4, 200)));
    }
}
