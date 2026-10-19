//! Screen command codecs and the software renderer.

mod dump;
mod message;
mod pixels;
mod renderer;

pub use dump::ScreenDump;
pub use message::{
    CacheAddition, CacheReference, ModernScreenFrameMessage, ModernScreenInitialization, ModernScreenLimits,
    ScreenError, ScreenRegion, Scroll,
};
pub use pixels::{bgr24_from_bgra, RectangleBufferError, ScreenRectangleBuffer};
pub use renderer::{ModernScreenBitmap, ModernScreenRenderer};
