//! Printer settings: the M503 command model and the static catalog of known
//! setting commands.

mod catalog;
mod command;
mod store;

pub(crate) use catalog::normalize_code;
pub use catalog::*;
pub use command::SettingCommand;
pub use store::{MotionLimits, PrinterSettings};
