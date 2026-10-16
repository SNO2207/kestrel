//! The media pass's thumbnail stage: the Gallery's thumbnail of each photo and video, made from
//! the picture the pass has already decoded (so a photo read for its text is not decoded again
//! by the shell) and filed in the app's thumbnail cache (`thumbs.rs`) under the key the
//! Gallery's `thumb` requests look for. Scrolling the Gallery then never waits for a render.

use kestrel_media::pipeline::{Input, Item, Needs, Stage};

use crate::thumbs::ThumbStore;

/// The size of the Gallery's thumbnails, square, in pixels; the Gallery is told it
/// (`GalleryData::thumb`) and asks the `thumb` protocol for the same.
pub const GALLERY_THUMB: u32 = 320;

pub struct ThumbnailStage {
    store: ThumbStore,
}

impl ThumbnailStage {
    pub const NAME: &'static str = "thumbnail";

    pub fn new(store: ThumbStore) -> Self {
        ThumbnailStage { store }
    }
}

impl Stage for ThumbnailStage {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        1
    }

    fn needs(&self) -> Needs {
        Needs { pixels: GALLERY_THUMB }
    }

    fn run(&self, item: &Item, input: &Input) -> Result<(), String> {
        // The row the Gallery shows carries the time in milliseconds, as the index has it.
        let mtime = kestrel_media::unix_ms(item.mtime);
        if self.store.has(&item.path, GALLERY_THUMB, mtime) {
            return Ok(());
        }
        let picture = input.picture.ok_or("No picture to make a thumbnail of")?;
        let jpeg = picture.fit(GALLERY_THUMB)?.to_jpeg()?;
        self.store.put(&item.path, GALLERY_THUMB, mtime, jpeg);
        Ok(())
    }
}
