use std::time::Instant;
use thiserror::Error;
use x11rb::errors;


#[derive(Debug, Clone)]
pub struct WindowKey {
    pub app_id: String,
    pub title: String,
    pub platform_id: String,
}

#[derive(Debug, Clone)]
pub struct FocusEvent {
    win_key: Option<WindowKey>,
    ts: Instant,
}



#[derive(Debug, Error)]
pub enum FocusError {
    #[error("X11 connection failed: {0}")]
    X11ConnectError(#[from] errors::ConnectionError),

}



pub trait FocusBackend {
    fn run() -> 
}
