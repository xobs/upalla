mod params;
mod plugin;
mod worker;

use params::UpallaParams;
use plugin::UpallaPlugin;
use std::sync::Arc;
use truce::prelude::*;

truce::plugin! {
    logic: UpallaPlugin,
    params: UpallaParams,
}
