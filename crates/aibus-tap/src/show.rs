//! The two texts: the list and one tapped session.

use crate::Env;

pub fn list(_env: &Env, _lister: &dyn Fn() -> Result<Vec<u8>, String>) -> Result<String, String> {
    Err("not implemented".into())
}

pub fn tap(
    _env: &Env,
    _lister: &dyn Fn() -> Result<Vec<u8>, String>,
    _arg: &str,
) -> Result<String, String> {
    Err("not implemented".into())
}
