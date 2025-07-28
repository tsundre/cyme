use cyme::display;
use cyme::profiler;

fn main() -> Result<(), String> {
    // get all system devices - use get_spusb_with_extra for verbose info
    let sp_usb = profiler::get_spusb()
        .map_err(|e| format!("Failed to gather system USB data from libusb, Error({e})"))?;

    // flatten since we don't care tree/buses
    let devices = sp_usb.flattened_devices();

    // print with default [`display::PrintSettings`]
    display::DisplayWriter::default()
        .print_flattened_devices(&devices, &display::PrintSettings::default());

    // alternatively iterate over devices and do something with them
    for device in devices {
        if let (Some(0x05ac), Some(_)) = (device.vendor_id, device.product_id) {
            println!("Found Apple device: {device}");
        }
    }

    Ok(())
}
