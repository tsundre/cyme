//! System USB profiler for getting system USB information, devices and descriptors
//!
//! Get [`SystemProfile`] struct of system USB buses and devices with extra data like configs, interfaces and endpoints. The mod function will be based on the feature enabled, either `libusb` or `nusb`. To use a specific profiler, see the submodules [`libusb`], [`nusb`] and [`macos`].
//!
//! ```no_run
//! use cyme::profiler;
//!
//! let spusb = profiler::get_spusb_with_extra().unwrap();
//! // print with alternative styling (#) is using utf-8 icons
//! println!("{:#}", spusb);
//! ```
//!
//! See [`types`] docs for what can be done with returned data, such as [`Filter`]
use crate::error::Result;
use itertools::Itertools;
use std::collections::HashMap;

use crate::error::{Error, ErrorKind};
#[cfg(all(target_os = "linux", any(feature = "udev", feature = "udevlib")))]
use crate::udev;
use crate::usb;

const REQUEST_GET_DESCRIPTOR: u8 = 0x06;
const REQUEST_GET_STATUS: u8 = 0x00;
const REQUEST_WEBUSB_URL: u8 = 0x02;

pub(crate) const SYSFS_USB_PREFIX: &str = "/sys/bus/usb/devices/";
pub(crate) const SYSFS_PCI_PREFIX: &str = "/sys/bus/pci/devices/";

// separate module but import all
pub mod types;
pub use types::*;

#[cfg(feature = "libusb")]
pub mod libusb;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(feature = "nusb")]
pub mod nusb;
#[cfg(all(feature = "nusb", feature = "watch"))]
pub mod watch;

/// Transfer direction
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Direction {
    /// Host to device
    Out = 0,
    /// Device to host
    In = 1,
}

/// Specification defining the request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum ControlType {
    /// Request defined by the USB standard.
    Standard = 0,
    /// Request defined by the standard USB class specification.
    Class = 1,
    /// Non-standard request.
    Vendor = 2,
}

/// Entity targeted by the request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Recipient {
    /// Request made to device as a whole.
    Device = 0,
    /// Request made to specific interface.
    Interface = 1,
    /// Request made to specific endpoint.
    Endpoint = 2,
    /// Other request.
    Other = 3,
}

/// Control request to USB device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ControlRequest {
    pub control_type: ControlType,
    pub recipient: Recipient,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: usize,
    pub claim_interface: bool,
}

/// Device USB operations required by the [`Profiler`]
pub(crate) trait UsbOperations {
    fn get_descriptor_string(&self, string_index: u8) -> Option<String>;
    fn get_control_msg(&self, control_request: ControlRequest) -> Result<Vec<u8>>;
}

/// OS level USB Profiler trait for profiling USB devices
pub(crate) trait Profiler<T>
where
    T: UsbOperations + std::fmt::Debug,
    Self: std::fmt::Debug,
{
    /// Get the USB HID Report Descriptor with a Control request
    fn get_report_descriptor(device: &T, index: u16, length: u16) -> Result<Vec<u8>> {
        let control_request = ControlRequest {
            control_type: ControlType::Standard,
            recipient: Recipient::Interface,
            request: REQUEST_GET_DESCRIPTOR,
            value: (u8::from(usb::DescriptorType::Report) as u16) << 8,
            index,
            length: length as usize,
            // only claim interface on linux
            claim_interface: cfg!(target_os = "linux") || cfg!(target_os = "android"),
        };
        device.get_control_msg(control_request)
    }

    /// Get the USB Hub Descriptor with a Control request, include hub port statuses
    fn get_hub_descriptor(
        device: &T,
        protocol: u8,
        bcd: u16,
        has_ssp: bool,
    ) -> Result<usb::HubDescriptor> {
        let is_ext_status = protocol == 3 && bcd >= 0x0310 && has_ssp;
        let value = if bcd >= 0x0300 {
            (u8::from(usb::DescriptorType::SuperSpeedHub) as u16) << 8
        } else {
            (u8::from(usb::DescriptorType::Hub) as u16) << 8
        };
        let control = ControlRequest {
            control_type: ControlType::Class,
            request: REQUEST_GET_DESCRIPTOR,
            value,
            index: 0,
            recipient: Recipient::Device,
            length: 12,
            claim_interface: false,
        };
        let data = match device.get_control_msg(control) {
            Ok(data) => data,
            Err(_) => {
                // if failed, try again with at least 9 bytes = min required for one port bitmask
                let control = ControlRequest {
                    control_type: ControlType::Class,
                    request: REQUEST_GET_DESCRIPTOR,
                    value,
                    index: 0,
                    recipient: Recipient::Device,
                    length: 9,
                    claim_interface: false,
                };
                device.get_control_msg(control)?
            }
        };
        let mut hub = usb::HubDescriptor::try_from(data.as_slice())?;

        // get port statuses
        let mut port_statues: Vec<[u8; 8]> = Vec::with_capacity(hub.num_ports as usize);
        for p in 0..hub.num_ports {
            // Request EXT_PORT_STATUS for USB 3.1 SuperSpeedPlus hubs, PORT_STATUS otherwise
            let control = ControlRequest {
                control_type: ControlType::Class,
                request: REQUEST_GET_STATUS,
                value: if is_ext_status { 2 } else { 0 },
                index: p as u16 + 1,
                recipient: Recipient::Other,
                length: if is_ext_status { 8 } else { 4 },
                claim_interface: false,
            };
            match device.get_control_msg(control) {
                Ok(mut data) => {
                    if data.len() < 8 {
                        let remaining = 8 - data.len();
                        data.extend(vec![0; remaining]);
                    }
                    port_statues.push(data.try_into().unwrap());
                }
                Err(e) => {
                    log::warn!(
                        "Failed to get port {} status for {:?}: {}",
                        p + 1,
                        device,
                        e
                    );
                    return Ok(hub);
                }
            }
        }

        hub.port_statuses = Some(port_statues);

        Ok(hub)
    }

    /// Get the USB Device status with a Control request
    fn get_device_status(device: &T) -> Result<u16> {
        let control = ControlRequest {
            control_type: ControlType::Standard,
            request: REQUEST_GET_STATUS,
            value: 0,
            index: 0,
            recipient: Recipient::Device,
            length: 2,
            claim_interface: false,
        };
        let data = device.get_control_msg(control)?;
        Ok(u16::from_le_bytes([data[0], data[1]]))
    }

    /// Get the USB Debug Descriptor with a Control request
    fn get_debug_descriptor(device: &T) -> Result<usb::DebugDescriptor> {
        let control = ControlRequest {
            control_type: ControlType::Standard,
            request: REQUEST_GET_DESCRIPTOR,
            value: (u8::from(usb::DescriptorType::Debug) as u16) << 8,
            index: 0,
            recipient: Recipient::Device,
            length: 2,
            // macOS seems to require claim to prevent timeout
            claim_interface: cfg!(target_os = "macos"),
        };
        let data = device.get_control_msg(control)?;
        usb::DebugDescriptor::try_from(data.as_slice())
    }

    /// Get the USB Device Binary Object Store (BOS) Descriptor with a Control request
    fn get_bos_descriptor(
        device: &T,
    ) -> Result<usb::descriptors::bos::BinaryObjectStoreDescriptor> {
        let mut control = ControlRequest {
            control_type: ControlType::Standard,
            request: REQUEST_GET_DESCRIPTOR,
            value: (u8::from(usb::DescriptorType::Bos) as u16) << 8,
            index: 0,
            recipient: Recipient::Device,
            length: 5,
            claim_interface: false,
        };
        let data = device.get_control_msg(control)?;
        let total_length = u16::from_le_bytes([data[2], data[3]]);
        log::debug!(
            "{device:?} Attempt read BOS descriptor total length: {total_length}"
        );
        // now get full descriptor
        control.length = total_length as usize;
        let data = device.get_control_msg(control)?;
        log::debug!("{device:?} BOS descriptor data: {data:?}");
        let mut bos =
            usb::descriptors::bos::BinaryObjectStoreDescriptor::try_from(data.as_slice())?;

        // get any extra descriptor data now with handle
        for c in bos.capabilities.iter_mut() {
            match c {
                usb::descriptors::bos::BosCapability::WebUsbPlatform(w) => {
                    w.url = Self::get_webusb_url(device, w.vendor_code, w.landing_page_index).ok();
                    log::trace!("{:?} WebUSB URL: {:?}", device, w.url);
                }
                usb::descriptors::bos::BosCapability::Billboard(ref mut b) => {
                    b.additional_info_url =
                        device.get_descriptor_string(b.additional_info_url_index);
                    for a in b.alternate_modes.iter_mut() {
                        a.alternate_mode_string =
                            device.get_descriptor_string(a.alternate_mode_string_index);
                    }
                }
                _ => (),
            }
        }

        Ok(bos)
    }

    /// Get the USB Device Qualifier Descriptor with a Control request
    fn get_device_qualifier(device: &T) -> Result<usb::DeviceQualifierDescriptor> {
        let control = ControlRequest {
            control_type: ControlType::Standard,
            request: REQUEST_GET_DESCRIPTOR,
            value: (u8::from(usb::DescriptorType::DeviceQualifier) as u16) << 8,
            index: 0,
            recipient: Recipient::Device,
            length: 10,
            claim_interface: false,
        };
        let data = device.get_control_msg(control)?;
        log::debug!("{device:?} Qualifier descriptor data: {data:?}");
        usb::DeviceQualifierDescriptor::try_from(data.as_slice())
    }

    /// Gets the WebUSB URL from the device, parsed and formatted as a URL
    ///
    /// https://github.com/gregkh/usbutils/blob/master/lsusb.c#L3261
    fn get_webusb_url(device: &T, vendor_request: u8, index: u8) -> Result<String> {
        let control = ControlRequest {
            control_type: ControlType::Vendor,
            request: vendor_request,
            value: index as u16,
            index: (REQUEST_WEBUSB_URL as u16) << 8,
            recipient: Recipient::Device,
            length: 3,
            claim_interface: false,
        };
        let data = device.get_control_msg(control)?;
        log::trace!("WebUSB URL descriptor data: {data:?}");
        let len = data[0] as usize;

        if data[1] != u8::from(usb::DescriptorType::String) {
            return Err(Error {
                kind: ErrorKind::Parsing,
                message: "Failed to parse WebUSB URL: Bad URL descriptor type".to_string(),
            });
        }

        if data.len() < len {
            return Err(Error {
                kind: ErrorKind::Parsing,
                message: "Failed to parse WebUSB URL: Data length mismatch".to_string(),
            });
        }

        let url = String::from_utf8(data[3..len].to_vec()).map_err(|e| Error {
            kind: ErrorKind::Parsing,
            message: format!("Failed to parse WebUSB URL: {e}"),
        })?;

        match data[2] {
            0x00 => Ok(format!("http://{url}")),
            0x01 => Ok(format!("https://{url}")),
            0xFF => Ok(url),
            _ => Err(Error {
                kind: ErrorKind::Parsing,
                message: "Failed to parse WebUSB URL: Bad URL scheme".to_string(),
            }),
        }
    }

    /// Build fully described USB device descriptor with extra bytes
    ///
    /// Fully described is based on the [`usb::ClassCodeTriplet`] and [`usb::Descriptor`] types. Any string indexes (or data which requires a control message) will be fetched and added to the descriptor while the device is still available.
    fn build_descriptor_extra<C: Into<usb::BaseClass> + Copy>(
        &self,
        device: &T,
        class_code: Option<usb::ClassCodeTriplet<C>>,
        interface_number: Option<u8>,
        extra_bytes: &[u8],
    ) -> Result<usb::Descriptor> {
        // Get any extra descriptors into a known type and add any handle data while we have it
        let mut dt = match usb::Descriptor::try_from(extra_bytes) {
            Ok(d) => d,
            Err(e) => {
                log::debug!(
                    "{device:?} Failed to convert extra descriptor bytes: {e}"
                );
                return Err(e);
            }
        };

        // Assign class context to interface since descriptor did not know it
        if let Some(interface_desc) = class_code {
            if let Err(e) = dt.update_with_class_context(interface_desc) {
                log::debug!(
                    "{device:?} Failed to update extra descriptor with class context: {e}"
                );
            }
        }

        // get any strings at string indexes while we have handle
        match dt {
            usb::Descriptor::InterfaceAssociation(ref mut iad) => {
                iad.function_string = device.get_descriptor_string(iad.function_string_index);
            }
            usb::Descriptor::Device(ref mut c)
            | usb::Descriptor::Interface(ref mut c)
            | usb::Descriptor::Endpoint(ref mut c) => match c {
                usb::ClassDescriptor::Printer(ref mut p) => {
                    for pd in p.descriptors.iter_mut() {
                        pd.uuid_string = device.get_descriptor_string(pd.uuid_string_index);
                    }
                }
                usb::ClassDescriptor::Communication(ref mut cdc) => match cdc.interface {
                    usb::descriptors::cdc::CdcInterfaceDescriptor::CountrySelection(ref mut d) => {
                        d.country_code_date =
                            device.get_descriptor_string(d.country_code_date_index);
                    }
                    usb::descriptors::cdc::CdcInterfaceDescriptor::NetworkChannel(ref mut d) => {
                        d.name = device.get_descriptor_string(d.name_string_index);
                    }
                    usb::descriptors::cdc::CdcInterfaceDescriptor::EthernetNetworking(
                        ref mut d,
                    ) => {
                        d.mac_address = device.get_descriptor_string(d.mac_address_index);
                    }
                    usb::descriptors::cdc::CdcInterfaceDescriptor::CommandSet(ref mut d) => {
                        d.command_set_string =
                            device.get_descriptor_string(d.command_set_string_index);
                    }
                    _ => (),
                },
                // grab report descriptor data using usb_control_msg
                usb::ClassDescriptor::Hid(ref mut hd) => {
                    for rd in hd.descriptors.iter_mut() {
                        if let Some(index) = interface_number {
                            rd.data =
                                Self::get_report_descriptor(device, index as u16, rd.length).ok();
                        }
                    }
                }
                usb::ClassDescriptor::Midi(ref mut md, _) => match md.interface {
                    usb::descriptors::audio::MidiInterfaceDescriptor::InputJack(ref mut mh) => {
                        mh.jack_string = device.get_descriptor_string(mh.jack_string_index);
                    }
                    usb::descriptors::audio::MidiInterfaceDescriptor::OutputJack(ref mut mh) => {
                        mh.jack_string = device.get_descriptor_string(mh.jack_string_index);
                    }
                    usb::descriptors::audio::MidiInterfaceDescriptor::Element(ref mut mh) => {
                        mh.element_string = device.get_descriptor_string(mh.element_string_index);
                    }
                    _ => (),
                },
                usb::ClassDescriptor::Audio(ref mut ad, _) => match ad.interface {
                    usb::descriptors::audio::UacInterfaceDescriptor::InputTerminal1(ref mut ah) => {
                        ah.channel_names = device.get_descriptor_string(ah.channel_names_index);
                        ah.terminal = device.get_descriptor_string(ah.terminal_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::InputTerminal2(ref mut ah) => {
                        ah.channel_names = device.get_descriptor_string(ah.channel_names_index);
                        ah.terminal = device.get_descriptor_string(ah.terminal_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::OutputTerminal1(
                        ref mut ah,
                    ) => {
                        ah.terminal = device.get_descriptor_string(ah.terminal_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::OutputTerminal2(
                        ref mut ah,
                    ) => {
                        ah.terminal = device.get_descriptor_string(ah.terminal_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::StreamingInterface2(
                        ref mut ah,
                    ) => {
                        ah.channel_names = device.get_descriptor_string(ah.channel_names_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::SelectorUnit1(ref mut ah) => {
                        ah.selector = device.get_descriptor_string(ah.selector_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::SelectorUnit2(ref mut ah) => {
                        ah.selector = device.get_descriptor_string(ah.selector_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::ProcessingUnit1(
                        ref mut ah,
                    ) => {
                        ah.channel_names = device.get_descriptor_string(ah.channel_names_index);
                        ah.processing = device.get_descriptor_string(ah.processing_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::ProcessingUnit2(
                        ref mut ah,
                    ) => {
                        ah.channel_names = device.get_descriptor_string(ah.channel_names_index);
                        ah.processing = device.get_descriptor_string(ah.processing_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::EffectUnit2(ref mut ah) => {
                        ah.effect = device.get_descriptor_string(ah.effect_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::FeatureUnit1(ref mut ah) => {
                        ah.feature = device.get_descriptor_string(ah.feature_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::FeatureUnit2(ref mut ah) => {
                        ah.feature = device.get_descriptor_string(ah.feature_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::ExtensionUnit1(ref mut ah) => {
                        ah.channel_names = device.get_descriptor_string(ah.channel_names_index);
                        ah.extension = device.get_descriptor_string(ah.extension_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::ExtensionUnit2(ref mut ah) => {
                        ah.channel_names = device.get_descriptor_string(ah.channel_names_index);
                        ah.extension = device.get_descriptor_string(ah.extension_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::ClockSource2(ref mut ah) => {
                        ah.clock_source = device.get_descriptor_string(ah.clock_source_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::ClockSelector2(ref mut ah) => {
                        ah.clock_selector = device.get_descriptor_string(ah.clock_selector_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::ClockMultiplier2(
                        ref mut ah,
                    ) => {
                        ah.clock_multiplier =
                            device.get_descriptor_string(ah.clock_multiplier_index);
                    }
                    usb::descriptors::audio::UacInterfaceDescriptor::SampleRateConverter2(
                        ref mut ah,
                    ) => {
                        ah.src = device.get_descriptor_string(ah.src_index);
                    }
                    _ => (),
                },
                usb::ClassDescriptor::Video(ref mut vd, _) => match vd.interface {
                    usb::descriptors::video::UvcInterfaceDescriptor::InputTerminal(ref mut vh) => {
                        vh.terminal = device.get_descriptor_string(vh.terminal_index);
                    }
                    usb::descriptors::video::UvcInterfaceDescriptor::OutputTerminal(ref mut vh) => {
                        vh.terminal = device.get_descriptor_string(vh.terminal_index);
                    }
                    usb::descriptors::video::UvcInterfaceDescriptor::SelectorUnit(ref mut vh) => {
                        vh.selector = device.get_descriptor_string(vh.selector_index);
                    }
                    usb::descriptors::video::UvcInterfaceDescriptor::ProcessingUnit(ref mut vh) => {
                        vh.processing = device.get_descriptor_string(vh.processing_index);
                    }
                    usb::descriptors::video::UvcInterfaceDescriptor::ExtensionUnit(ref mut vh) => {
                        vh.extension = device.get_descriptor_string(vh.extension_index);
                    }
                    usb::descriptors::video::UvcInterfaceDescriptor::EncodingUnit(ref mut vh) => {
                        vh.encoding = device.get_descriptor_string(vh.encoding_index);
                    }
                    _ => (),
                },
                _ => (),
            },
            _ => (),
        }

        Ok(dt)
    }

    /// Build [`usb::Descriptor`]s from extra bytes of a Configuration Descriptor
    fn build_config_descriptor_extra(
        &self,
        device: &T,
        mut raw: Vec<u8>,
    ) -> Result<Vec<usb::Descriptor>> {
        let extra_len = raw.len();
        let mut taken = 0;
        let mut ret = Vec::new();

        // Iterate on chunks of the header length
        while taken < extra_len && extra_len >= 2 {
            let dt_len = raw[0] as usize;
            let dt = self.build_descriptor_extra::<u8>(
                device,
                None,
                None,
                &raw.drain(..dt_len).collect::<Vec<u8>>(),
            )?;
            log::debug!("{device:?} Config descriptor extra: {dt:?}");
            ret.push(dt);
            taken += dt_len;
        }

        Ok(ret)
    }

    /// Build [`usb::Descriptor`]s from extra bytes of an Interface Descriptor
    fn build_interface_descriptor_extra<C: Into<usb::BaseClass> + Copy>(
        &self,
        device: &T,
        class_code: usb::ClassCodeTriplet<C>,
        interface_number: u8,
        mut raw: Vec<u8>,
    ) -> Result<Vec<usb::Descriptor>> {
        let extra_len = raw.len();
        let mut taken = 0;
        let mut ret = Vec::new();

        // Iterate on chunks of the header length
        while taken < extra_len && extra_len >= 2 {
            let dt_len = raw[0] as usize;
            let dt = self.build_descriptor_extra(
                device,
                Some(class_code),
                Some(interface_number),
                &raw.drain(..dt_len).collect::<Vec<u8>>(),
            )?;

            log::debug!("{device:?} Interface descriptor extra: {dt:?}");
            ret.push(dt);
            taken += dt_len;
        }

        Ok(ret)
    }

    /// Build [`usb::Descriptor`]s from extra bytes of an Endpoint Descriptor
    fn build_endpoint_descriptor_extra<C: Into<usb::BaseClass> + Copy>(
        &self,
        device: &T,
        class_code: usb::ClassCodeTriplet<C>,
        interface_number: u8,
        mut raw: Vec<u8>,
    ) -> Result<Option<Vec<usb::Descriptor>>> {
        let extra_len = raw.len();
        let mut taken = 0;
        let mut ret = Vec::new();

        // Iterate on chunks of the header length
        while taken < extra_len && extra_len >= 2 {
            let dt_len = raw[0] as usize;
            let dt = self.build_descriptor_extra(
                device,
                Some(class_code),
                Some(interface_number),
                &raw.drain(..dt_len).collect::<Vec<u8>>(),
            )?;

            log::debug!("{device:?} Endpoint descriptor extra: {dt:?}");
            ret.push(dt);
            taken += dt_len;
        }

        Ok(Some(ret))
    }

    /// Get [`Device`]s connected to the host, excluding root hubs
    fn get_devices(&mut self, with_extra: bool) -> Result<Vec<Device>>;

    /// Get root hubs connected to the host as [`Device`]s
    ///
    /// root hubs are pseudo devices and not always listed in the device list, so this is a separate function to get them. The data is used to help create [`Bus`]es; root hubs are an abstraction over Host Controller information.
    fn get_root_hubs(&mut self) -> Result<HashMap<u8, Device>>;

    /// Get the [`Bus`]s connected to the host for building the [`SystemProfile`]
    fn get_buses(&mut self) -> Result<HashMap<u8, Bus>>;

    /// Create a new [`Bus`] from a root hub [`Device`]
    fn new_sp_bus(&self, bus_number: u8, root_hub: Option<Device>) -> Bus {
        root_hub
            .map(|rh| {
                rh.try_into().unwrap_or_else(|e| {
                    log::warn!("Failed to convert root hub to Bus: {e:?}");
                    Bus::from(bus_number)
                })
            })
            .unwrap_or(Bus::from(bus_number))
    }

    /// Build the [`SystemProfile`] from the Profiler get_devices and get_root_hubs (for buses) functions
    fn get_spusb(&mut self, with_extra: bool) -> Result<SystemProfile> {
        let mut spusb = SystemProfile { buses: Vec::new() };

        log::info!("Building SystemProfile with {self:?}");

        // temporary store of devices created when iterating through DeviceList
        let mut cache = self.get_devices(with_extra)?;
        cache.sort_by_key(|d| d.location_id.bus);
        log::trace!("Sorted devices {cache:#?}");
        // get system buses
        let mut buses = self.get_buses()?;
        log::trace!("Buses {buses:#?}");

        // group by bus number and then stick them into a bus in the returned SystemProfile
        for (key, group) in &cache.into_iter().group_by(|d| d.location_id.bus) {
            // create the bus if missing, we'll add devices at next step
            let mut new_bus = buses.remove(&key).unwrap_or(Bus::from(key));

            // group into parent groups with parent path as key or trunk devices so they end up in same place
            let parent_groups =
                group.group_by(|d| d.parent_port_path().unwrap_or(d.trunk_port_path()));

            // now go through parent paths inserting devices owned by that parent
            // this is not perfect...if the sort of devices does not result in order of depth, it will panic because the parent of a device will not exist. But that won't happen, right...
            // sort key - ends_with to ensure root_hubs, which will have same str length as trunk devices will still be ahead
            for (parent_path, children) in parent_groups.into_iter().sorted_by_key(|x| x.0.depth())
            {
                // if root devices, add them to bus
                if parent_path.is_root_hub() {
                    // if parent_path == "-" {
                    let devices = std::mem::take(&mut new_bus.devices);
                    if let Some(mut d) = devices {
                        for new_device in children {
                            d.push(new_device);
                        }
                        new_bus.devices = Some(d);
                    } else {
                        new_bus.devices = Some(children.collect());
                    }
                    // else find and add parent - this should work because we are sorted to accend the tree so parents should be created before their children
                } else {
                    let parent_node = new_bus
                        .get_node_mut(&parent_path)
                        .expect("Parent node does not exist in new bus!");
                    let devices = std::mem::take(&mut parent_node.devices);
                    if let Some(mut d) = devices {
                        for new_device in children {
                            d.push(new_device);
                        }
                        parent_node.devices = Some(d);
                    } else {
                        parent_node.devices = Some(children.collect());
                    }
                }
            }

            spusb.buses.push(new_bus);
        }

        // add empty buses if missing
        if !buses.is_empty() {
            for (_, bus) in buses {
                spusb.buses.push(bus);
            }
            spusb.buses.sort_by_key(|b| b.usb_bus_number);
        }

        Ok(spusb)
    }

    /// Fills a passed mutable `spusb` reference to fill using `get_spusb`. Will replace existing [`Device`]s found in the Profiler tree but leave others and the buses.
    ///
    /// The main use case for this is to merge with macOS `system_profiler` data, so that [`usb::DeviceExtra`] can be obtained but internal buses kept. One could also use it to update a static .json dump.
    fn fill_spusb(&mut self, spusb: &mut SystemProfile) -> Result<()> {
        let libusb_spusb = self.get_spusb(true)?;

        // merge if passed has any buses
        if !spusb.buses.is_empty() {
            for mut bus in libusb_spusb.buses {
                if let Some(existing) = spusb
                    .buses
                    .iter_mut()
                    .find(|b| b.get_bus_number() == bus.get_bus_number())
                {
                    // just take the devices and put them in since nusb/libusb will be more verbose
                    // bus macOS profiler will have accurate bus information
                    existing.devices = std::mem::take(&mut bus.devices);
                }
            }
        }

        Ok(())
    }
}

/// Get a USB device attribute String from sysfs on Linux
#[allow(unused_variables)]
fn get_sysfs_string(sysfs_name: &str, attr: &str) -> Option<String> {
    log::trace!("Getting sysfs string at {sysfs_name}/{attr}");
    #[cfg(any(target_os = "linux", target_os = "android"))]
    return std::fs::read_to_string(format!("{}{}/{}", SYSFS_USB_PREFIX, sysfs_name, attr))
        .ok()
        .map(|s| s.trim().to_string());
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    return None;
}

#[allow(unused_variables)]
fn get_sysfs_readlink(sysfs_name: &str, attr: &str) -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // switch based on root_hub - if it is a root hub, we need to go up a directory to get the pci driver
        // https://github.com/gregkh/usbutils/blob/cda6883cade6ec67671d0c7de61e70eb992509a9/lsusb-t.c#L434
        let path = if sysfs_name.starts_with("usb") && attr == "driver" {
            format!("{}{}/../{}", SYSFS_USB_PREFIX, sysfs_name, attr)
        } else {
            format!("{}{}/{}", SYSFS_USB_PREFIX, sysfs_name, attr)
        };

        log::trace!("readlink at {}", path);
        std::fs::read_link(path)
            .ok()
            .and_then(|s| s.file_name().map(|f| f.to_string_lossy().to_string()))
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    return None;
}

/// Get the USB driver name from udev on Linux if the feature is enabled
#[allow(unused_variables)]
fn get_udev_driver_name(port_path: &str) -> Result<Option<String>> {
    #[cfg(all(target_os = "linux", any(feature = "udev", feature = "udevlib")))]
    return udev::get_udev_driver_name(port_path);
    #[cfg(not(all(target_os = "linux", any(feature = "udev", feature = "udevlib"))))]
    return Ok(None);
}

/// Get the USB device syspath from udev on Linux if the feature is enabled
#[allow(unused_variables)]
fn get_udev_syspath(port_path: &str) -> Result<Option<String>> {
    #[cfg(all(target_os = "linux", any(feature = "udev", feature = "udevlib")))]
    return udev::get_udev_syspath(port_path);
    #[cfg(not(all(target_os = "linux", any(feature = "udev", feature = "udevlib"))))]
    return Ok(None);
}

/// Get the USB device syspath based on the default location "/sys/bus/usb/devices" on Linux
#[allow(unused_variables)]
fn get_syspath(port_path: &str) -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    return Some(format!("{}{}", SYSFS_USB_PREFIX, port_path));
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    return None;
}

/// Build [`SystemProfile`] by profiling system. Does not source [`usb::DeviceExtra`] - use [`get_spusb_with_extra`] for that; the extra operation is mostly moving data around so the only hit is to stack.
///
/// Runs through [`Profiler::get_devices()`] creating a cache of [`Device`]. Then sorts into parent groups, where the [`Bus`] is created -  with root hub information if available from [`Profiler::get_root_hubs()`] - and the tree built.
///
/// The function will call which library is available based on the features enabled: 'nusb' or 'libusb'. If neither are enabled, it will return an error.If both are enabled, it will default to 'nusb'.
///
/// Bus data on Windows is only available with 'nusb', and on this bus numbers are created in order of appearance since it is not a concept in the Windows USB stack.
pub fn get_spusb() -> Result<SystemProfile> {
    #[cfg(all(feature = "libusb", not(feature = "nusb")))]
    {
        let mut profiler = libusb::LibUsbProfiler;
        <libusb::LibUsbProfiler as Profiler<libusb::UsbDevice<rusb::Context>>>::get_spusb(
            &mut profiler,
            false,
        )
    }
    #[cfg(feature = "nusb")]
    {
        let mut profiler = nusb::NusbProfiler::new();
        profiler.get_spusb(true)
    }

    #[cfg(all(not(feature = "libusb"), not(feature = "nusb")))]
    {
        Err(crate::error::Error::new(
            crate::error::ErrorKind::Unsupported,
            "nusb or libusb feature is required to do this, install with `cargo install --features nusb/libusb`",
        ))
    }
}

/// Build [`SystemProfile`] including [`usb::DeviceExtra`] - the main function to use for most use cases unless one does not want verbose data. The extra data requires opening the device to read device descriptors.
///
/// See [`Profiler::get_spusb()`] for more information.
pub fn get_spusb_with_extra() -> Result<SystemProfile> {
    #[cfg(all(feature = "libusb", not(feature = "nusb")))]
    {
        let mut profiler = libusb::LibUsbProfiler;
        <libusb::LibUsbProfiler as Profiler<libusb::UsbDevice<rusb::Context>>>::get_spusb(
            &mut profiler,
            true,
        )
    }

    #[cfg(feature = "nusb")]
    {
        let mut profiler = nusb::NusbProfiler::new();
        profiler.get_spusb(true)
    }

    #[cfg(all(not(feature = "libusb"), not(feature = "nusb")))]
    {
        Err(crate::error::Error::new(
            crate::error::ErrorKind::Unsupported,
            "nusb or libusb feature is required to do this, install with `cargo install --features nusb/libusb`",
        ))
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::ffi::{OsStr, OsString};

    /// Parse VID, PID, revision, subsys and ID from a Host Controller ID: https://learn.microsoft.com/en-us/windows-hardware/drivers/install/identifiers-for-pci-devices
    ///
    /// The subsys is a 32-bit value with SID in the high 16 bits and CID in the low 16 bits.
    fn parse_host_controller_id(s: &OsStr) -> Option<(u16, u16, u8, u32, Option<String>)> {
        let s = s.to_str()?;
        let s = s.strip_prefix("PCI\\VEN_")?;
        let vid = u16::from_str_radix(s.get(0..4)?, 16).ok()?;
        let s = s.get(4..)?.strip_prefix("&DEV_")?;
        let pid = u16::from_str_radix(s.get(0..4)?, 16).ok()?;
        let s = s.get(4..)?.strip_prefix("&SUBSYS_")?;
        let sidcid = u32::from_str_radix(s.get(0..8)?, 16).ok()?;
        let s = s.get(8..)?.strip_prefix("&REV_")?;
        let rev = u8::from_str_radix(s.get(0..2)?, 16).ok()?;
        let id = s.get(2..)?.strip_prefix("\\").map(|s| s.to_owned());
        Some((vid, pid, rev, sidcid, id))
    }

    pub(crate) fn pci_info_from_parent(pci_path: &OsStr) -> Option<PciInfo> {
        let pci_id = parse_host_controller_id(pci_path)?;

        Some(PciInfo {
            vendor_id: pci_id.0,
            product_id: pci_id.1,
            revision: pci_id.2 as u16,
        })
    }

    pub(crate) fn pci_info_from_device(device: &Device) -> Option<PciInfo> {
        device
            .serial_num
            .as_ref()
            .and_then(|s| pci_info_from_parent(&OsString::from(s)))
    }

    #[cfg(feature = "nusb")]
    pub(crate) fn pci_info_from_bus(bus_info: &::nusb::BusInfo) -> Option<PciInfo> {
        pci_info_from_parent(bus_info.parent_instance_id())
    }

    #[cfg(feature = "nusb")]
    pub(crate) fn from(bus: &::nusb::BusInfo) -> Bus {
        if let Some(pci_info) = platform::pci_info_from_bus(bus) {
            let (host_controller_vendor, host_controller_device) =
                match pci_ids::Device::from_vid_pid(pci_info.vendor_id, pci_info.product_id) {
                    Some(d) => (
                        Some(d.vendor().name().to_string()),
                        Some(d.name().to_string()),
                    ),
                    None => (None, None),
                };

            Bus {
                usb_bus_number: None,
                name: bus.system_name().map(|s| s.to_string()).unwrap_or_default(),
                host_controller: bus.parent_instance_id().to_string_lossy().to_string(),
                host_controller_vendor,
                host_controller_device,
                pci_vendor: Some(pci_info.vendor_id),
                pci_device: Some(pci_info.product_id),
                pci_revision: Some(pci_info.revision),
                id: bus.bus_id().to_string(),
                ..Default::default()
            }
        } else {
            Bus {
                usb_bus_number: None,
                name: bus.system_name().map(|s| s.to_string()).unwrap_or_default(),
                host_controller: bus.parent_instance_id().to_string_lossy().to_string(),
                id: bus.bus_id().to_string(),
                ..Default::default()
            }
        }
    }

    #[test]
    fn test_parse_host_controller_id() {
        assert_eq!(parse_host_controller_id(OsStr::new("")), None);
        assert_eq!(
            parse_host_controller_id(OsStr::new(
                "PCI\\VEN_8086&DEV_2658&SUBSYS_04001AB8&REV_02\\3&11583659&0&E8"
            )),
            Some((
                0x8086,
                0x2658,
                2,
                0x04001AB8,
                Some("3&11583659&0&E8".to_string())
            ))
        );
        assert_eq!(
            parse_host_controller_id(OsStr::new("PCI\\VEN_8086&DEV_2658")),
            None
        );
        assert_eq!(
            parse_host_controller_id(OsStr::new("PCI\\VEN_8086&DEV_2658&SUBSYS_04001AB8&REV_02")),
            Some((0x8086, 0x2658, 2, 0x04001AB8, None))
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod platform {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::str::FromStr;

    // SysfsPath, parsing etc is taken from nusb crate (since not pub) and modified for our use
    #[derive(Debug, Clone)]
    struct SysfsPath(pub(crate) PathBuf);

    impl SysfsPath {
        pub(crate) fn exists(&self) -> bool {
            self.0.exists()
        }

        fn parse_attr<T>(&self, attr: &str, parse: impl FnOnce(&str) -> Result<T>) -> Result<T> {
            let attr_path = self.0.join(attr);
            fs::read_to_string(&attr_path)
                .map_err(|e| Error::new(ErrorKind::Io, &e.to_string()))
                .and_then(|v| parse(v.trim()))
        }

        pub(crate) fn read_attr<T: FromStr>(&self, attr: &str) -> Result<T> {
            self.parse_attr(attr, |s| {
                s.parse().map_err(|_| {
                    Error::new(
                        ErrorKind::Parsing,
                        &format!("Failed to parse attr: {}", attr),
                    )
                })
            })
        }

        fn read_attr_hex<T: FromHexStr>(&self, attr: &str) -> Result<T> {
            self.parse_attr(attr, |s| T::from_hex_str(s.strip_prefix("0x").unwrap_or(s)))
        }

        fn children(&self) -> impl Iterator<Item = SysfsPath> {
            fs::read_dir(&self.0)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|f| f.ok())
                .filter(|f| f.file_type().ok().is_some_and(|t| t.is_dir()))
                .map(|f| SysfsPath(f.path()))
        }
    }

    trait FromHexStr: Sized {
        fn from_hex_str(s: &str) -> Result<Self>;
    }

    impl FromHexStr for u8 {
        fn from_hex_str(s: &str) -> Result<Self> {
            u8::from_str_radix(s, 16).map_err(|_| Error::new(ErrorKind::Parsing, s))
        }
    }

    impl FromHexStr for u16 {
        fn from_hex_str(s: &str) -> Result<Self> {
            u16::from_str_radix(s, 16).map_err(|_| Error::new(ErrorKind::Parsing, s))
        }
    }

    impl std::fmt::Display for SysfsPath {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            self.0.display().fmt(f)
        }
    }

    impl From<&str> for SysfsPath {
        fn from(s: &str) -> Self {
            SysfsPath(PathBuf::from(s))
        }
    }

    impl From<String> for SysfsPath {
        fn from(s: String) -> Self {
            SysfsPath(PathBuf::from(s))
        }
    }

    impl From<PathBuf> for SysfsPath {
        fn from(p: PathBuf) -> Self {
            SysfsPath(p)
        }
    }

    impl std::ops::Deref for SysfsPath {
        type Target = PathBuf;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl From<SysfsPath> for PathBuf {
        fn from(s: SysfsPath) -> Self {
            s.0
        }
    }

    fn pci_info_from_parent(pci_path: &SysfsPath) -> Option<PciInfo> {
        Some(PciInfo {
            vendor_id: pci_path.read_attr_hex("vendor").ok()?,
            product_id: pci_path.read_attr_hex("device").ok()?,
            revision: pci_path.read_attr_hex("revision").ok()?,
        })
    }

    pub(crate) fn pci_info_from_device(device: &Device) -> Option<PciInfo> {
        device.serial_num.as_ref().and_then(|s| {
            let pci_path = SysfsPath::from(PathBuf::from(SYSFS_PCI_PREFIX).join(s));
            log::debug!("Probing device {:?}", pci_path);
            pci_info_from_parent(&pci_path)
        })
    }

    #[cfg(feature = "nusb")]
    pub(crate) fn pci_info_from_bus(bus_info: &::nusb::BusInfo) -> Option<PciInfo> {
        let path = bus_info.sysfs_path();
        let parent_path = path
            .parent()
            .and_then(|p| p.to_str())
            .map(|s| s.to_string())?;
        let pci_path = SysfsPath::from(PathBuf::from(SYSFS_PCI_PREFIX).join(parent_path));
        log::debug!("Probing bus parent device {:?}", pci_path);
        pci_info_from_parent(&pci_path)
    }

    #[cfg(feature = "nusb")]
    pub(crate) fn from(bus: &::nusb::BusInfo) -> Bus {
        if let Some(pci_info) = platform::pci_info_from_bus(bus) {
            let (host_controller_vendor, host_controller_device) =
                match pci_ids::Device::from_vid_pid(pci_info.vendor_id, pci_info.product_id) {
                    Some(d) => (
                        Some(d.vendor().name().to_string()),
                        Some(d.name().to_string()),
                    ),
                    None => (None, None),
                };

            Bus {
                usb_bus_number: Some(bus.bus_id().parse::<u8>().expect(
                    "Failed to parse bus_id: Linux bus_id should be a decimal string and not None",
                )),
                name: bus.system_name().map(|s| s.to_string()).unwrap_or_default(),
                host_controller: bus
                    .root_hub()
                    .manufacturer_string()
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                host_controller_vendor,
                host_controller_device,
                pci_vendor: Some(pci_info.vendor_id),
                pci_device: Some(pci_info.product_id),
                pci_revision: Some(pci_info.revision),
                ..Default::default()
            }
        } else {
            Bus {
                usb_bus_number: Some(bus.bus_id().parse::<u8>().expect(
                    "Failed to parse bus_id: Linux bus_id should be a decimal string and not None",
                )),
                name: bus.system_name().map(|s| s.to_string()).unwrap_or_default(),
                host_controller: bus
                    .root_hub()
                    .manufacturer_string()
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                ..Default::default()
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use macos::HostControllerInfo;

    impl From<HostControllerInfo> for PciInfo {
        fn from(pci_info: HostControllerInfo) -> Self {
            PciInfo {
                vendor_id: pci_info.vendor_id,
                product_id: pci_info.device_id,
                revision: pci_info.revision_id,
            }
        }
    }

    #[allow(unused_variables)]
    pub(crate) fn pci_info_from_device(device: &Device) -> Option<PciInfo> {
        None
    }

    #[cfg(feature = "nusb")]
    pub(crate) fn pci_info_from_bus(bus_info: &::nusb::BusInfo) -> Option<PciInfo> {
        bus_info
            .name()
            .and_then(|name| macos::get_controller(name).ok().map(|c| c.into()))
    }

    #[cfg(feature = "nusb")]
    pub(crate) fn from(bus: &::nusb::BusInfo) -> Bus {
        if let Some(pci_info) = platform::pci_info_from_bus(bus) {
            let (host_controller_vendor, host_controller_device) =
                match pci_ids::Device::from_vid_pid(pci_info.vendor_id, pci_info.product_id) {
                    Some(d) => (
                        Some(d.vendor().name().to_string()),
                        Some(d.name().to_string()),
                    ),
                    None => (None, None),
                };

            Bus {
                usb_bus_number: Some(u8::from_str_radix(bus.bus_id(), 16).expect("Failed to parse bus_id: macOS bus_id should be a hexadecimal string and not None")),
                name: bus.class_name().to_string(),
                host_controller: bus.provider_class_name().to_string(),
                host_controller_vendor,
                host_controller_device,
                pci_vendor: Some(pci_info.vendor_id),
                pci_device: Some(pci_info.product_id),
                pci_revision: Some(pci_info.revision),
                ..Default::default()
            }
        } else {
            Bus {
                usb_bus_number: Some(u8::from_str_radix(bus.bus_id(), 16).expect("Failed to parse bus_id: macOS bus_id should be a hexadecimal string and not None")),
                name: bus.class_name().to_string(),
                host_controller: bus.provider_class_name().to_string(),
                ..Default::default()
            }
        }
    }
}
