// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The OS clipboard outside egui (images, key wiping). Without `desktop` (browser build) there is
//! none: `new` fails and callers skip; egui's own copy/paste still works in the page.

#[cfg(feature = "desktop")]
pub use arboard::Clipboard;

#[cfg(not(feature = "desktop"))]
pub struct Clipboard;

#[cfg(not(feature = "desktop"))]
pub struct ImageData {
    pub width: usize,
    pub height: usize,
    pub bytes: std::borrow::Cow<'static, [u8]>,
}

#[cfg(not(feature = "desktop"))]
impl Clipboard {
    pub fn new() -> Result<Self, ()> {
        Err(())
    }
    pub fn get_text(&mut self) -> Result<String, ()> {
        Err(())
    }
    pub fn set_text(&mut self, _: String) -> Result<(), ()> {
        Err(())
    }
    pub fn get_image(&mut self) -> Result<ImageData, ()> {
        Err(())
    }
}
