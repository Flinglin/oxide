use gpui::{GpuSpecs, Window};
use oxide_render_policy::DetectedGraphics;

pub fn detect_graphics(window: &Window) -> DetectedGraphics {
    detected_graphics_from_specs(window.gpu_specs())
}

fn detected_graphics_from_specs(specs: Option<GpuSpecs>) -> DetectedGraphics {
    if let Some(specs) = specs {
        if specs.is_software_emulated {
            DetectedGraphics::software_emulated(
                specs.device_name,
                specs.driver_name,
                specs.driver_info,
            )
        } else if specs.is_virtual_gpu {
            DetectedGraphics::virtual_gpu(specs.device_name, specs.driver_name, specs.driver_info)
        } else {
            DetectedGraphics::hardware(specs.device_name, specs.driver_name, specs.driver_info)
        }
    } else {
        DetectedGraphics::unknown_hardware()
    }
}
