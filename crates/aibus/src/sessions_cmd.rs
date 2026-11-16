//! `aibus sessions` (#355).

use aibus_core::{AibusError, Redactor};

use crate::cli::{Global, SessionsArgs};

pub fn sessions(_args: &SessionsArgs, _g: &Global, _redactor: &Redactor) -> Result<u8, AibusError> {
    Err(AibusError::Usage("not implemented".into()))
}
