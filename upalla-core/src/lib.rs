pub mod ort_tract;
pub mod wav;

const CONFIG_INI: &str = include_str!("../config.ini");

pub fn load_config() -> anyhow::Result<ini::Ini> {
    Ok(ini::Ini::load_from_str(CONFIG_INI)?)
}
