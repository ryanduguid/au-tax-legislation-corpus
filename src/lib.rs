//! The au-tax-legislation-corpus tooling ported from Python.
//!
//! Stage 1 ports the change-review radar (`tax-radar-au`). The corpus builder
//! stages remain in the Python `fadden` package until they are ported.
pub mod argparse;
mod argtheme;
pub mod fadden;
pub mod py;
pub mod pyio;
pub mod pyjson;
pub mod pyos;
pub mod pypath;
mod pytext;
mod pyunicode;
pub mod pyurl;
pub mod pyvalue;
pub mod radar;
mod termsize;
mod textwrap;
