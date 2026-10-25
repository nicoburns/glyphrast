//! LRU cache of configured [`skrifa::ScalerFont`]s.

use crate::{FontKey, Hinting};
use alloc::vec::Vec;
use read_fonts::model::{Font, NormalizedCoord};
use skrifa::ScalerFont;
use smallvec::SmallVec;

/// Everything that selects a scaler: font file, variation location, size and hinting.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ScalerKey {
    pub font: FontKey,
    pub coords: SmallVec<[NormalizedCoord; 8]>,
    pub ppem_bits: u32,
    pub hinting: Hinting,
}

struct Entry {
    key: ScalerKey,
    scaler: ScalerFont,
    serial: u64,
}

pub(crate) struct ScalerCache {
    entries: Vec<Entry>,
    max_entries: usize,
    serial: u64,
}

impl ScalerCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_entries,
            serial: 0,
        }
    }

    /// Returns the scaler for `key`, building it from `instance` on a miss.
    ///
    /// `instance` yields the font at the key's variation location. The hinted
    /// scaler falls back to an unhinted one when the font's hinting programs
    /// fail, as swash did when `HintingInstance::new` returned an error.
    pub fn get(&mut self, key: &ScalerKey, instance: impl FnOnce() -> Font) -> &ScalerFont {
        self.serial += 1;
        let serial = self.serial;
        let mut lru = 0;
        let mut lru_serial = u64::MAX;
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if entry.key == *key {
                entry.serial = serial;
                return &self.entries[i].scaler;
            }
            if entry.serial < lru_serial {
                lru_serial = entry.serial;
                lru = i;
            }
        }
        let font = instance();
        let ppem = f32::from_bits(key.ppem_bits);
        let hint_options = (ppem != 0.).then(|| key.hinting.options()).flatten();
        if self.entries.len() < self.max_entries {
            let scaler = match hint_options {
                Some(options) => ScalerFont::hinted(font.clone(), ppem, options),
                None => ScalerFont::force_unhinted(font.clone(), (ppem != 0.).then_some(ppem)),
            };
            let scaler = unhinted_on_error(scaler, &font, ppem);
            self.entries.push(Entry {
                key: key.clone(),
                scaler,
                serial,
            });
            return &self.entries.last().unwrap().scaler;
        }
        let entry = &mut self.entries[lru];
        entry.key = key.clone();
        entry.serial = serial;
        match hint_options {
            Some(options) => {
                entry.scaler.reconfigure(font.clone(), ppem, options);
                if entry.scaler.hinting_error().is_some() {
                    entry.scaler = ScalerFont::force_unhinted(font, Some(ppem));
                }
            }
            None => {
                entry.scaler = ScalerFont::force_unhinted(font, (ppem != 0.).then_some(ppem));
            }
        }
        &entry.scaler
    }
}

fn unhinted_on_error(scaler: ScalerFont, font: &Font, ppem: f32) -> ScalerFont {
    if scaler.hinting_error().is_some() {
        ScalerFont::force_unhinted(font.clone(), Some(ppem))
    } else {
        scaler
    }
}
