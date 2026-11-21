mod audio_features;
mod definitions;
mod devices_info;
mod utils;

pub use self::audio_features::*;
pub use self::definitions::*;

/// Query Android's native mixer output sample rate (`PROPERTY_OUTPUT_SAMPLE_RATE`).
pub fn native_output_sample_rate() -> Option<u32> {
    let context = utils::get_context();
    utils::with_attached(context, |env, context| {
        let audio_manager = utils::get_system_service(env, &context, Context::AUDIO_SERVICE)?;
        let value = utils::get_property(
            env,
            &audio_manager,
            AudioManager::PROPERTY_OUTPUT_SAMPLE_RATE,
        )?;
        if value.is_null() {
            return Ok(None);
        }
        let text = String::from(env.get_string(&value)?);
        Ok(text.parse::<u32>().ok().filter(|&rate| rate > 0))
    })
    .ok()
    .flatten()
}
