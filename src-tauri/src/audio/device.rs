use cpal::traits::{DeviceTrait, HostTrait};

use crate::error::{AppError, AppResult};
use crate::model::OutputDevice;

pub fn list_output_devices() -> AppResult<Vec<OutputDevice>> {
    let host = cpal::default_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());
    let mut out: Vec<OutputDevice> = Vec::new();
    for device in host.output_devices().map_err(|e| AppError::Audio(e.to_string()))? {
        let Ok(name) = device.name() else { continue };
        if out.iter().any(|d| d.name == name) {
            continue;
        }
        out.push(OutputDevice { is_default: default_name.as_deref() == Some(name.as_str()), name });
    }
    Ok(out)
}

/// Opens the named device, falling back to the system default if it is gone.
pub fn open_output_device(name: Option<&str>) -> AppResult<cpal::Device> {
    let host = cpal::default_host();
    if let Some(wanted) = name {
        if let Ok(mut devices) = host.output_devices() {
            if let Some(d) = devices.find(|d| d.name().ok().as_deref() == Some(wanted)) {
                return Ok(d);
            }
        }
        tracing::warn!(device = wanted, "configured output device not found; using system default");
    }
    host.default_output_device().ok_or_else(|| AppError::Audio("no audio output device available".into()))
}
