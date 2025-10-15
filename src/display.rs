//! Provides the main utilities to display USB types within this crate - primarily used by `cyme` binary.
//!
//! TODO: There is some repeat code that could probably be made into functions/generics
use clap::ValueEnum;
use colored::*;
use fastrand;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use std::cmp;
use std::collections::HashMap;
use std::hash::Hash;
use std::io::{self, Write};
use strum::{IntoEnumIterator, VariantArray};
use strum_macros::{Display, EnumIter, VariantArray};
use unicode_width::UnicodeWidthStr;

use crate::colour;
use crate::error::Result;
use crate::icon;
use crate::profiler::{Bus, Device, Filter, SystemProfile};
use crate::types::NumericalUnit;
use crate::usb::{
    path::ConfigurationPath, path::DevicePath, path::EndpointPath, path::PortPath,
    ConfigAttributes, Configuration, DeviceExtra, Direction, Endpoint, Interface,
};

const ICON_HEADING: &str = "I";
const DEFAULT_AUTO_WIDTH: u16 = 80; // default terminal width to scale if None returned for size
const MIN_VARIABLE_STRING_LEN: usize = 5; // minimum variable string length to scale to
const LIST_INSET_SPACES: u8 = 2; // number of spaces for non-tree inset

/// Colouring control for the output
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ColorWhen {
    /// Show colours if the output goes to an interactive console
    #[default]
    Auto,
    /// Always apply colouring to the output
    Always,
    /// Never apply colouring to the output
    Never,
}

impl std::fmt::Display for ColorWhen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Icon control for the output
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "kebab-case")]
pub enum IconWhen {
    /// Show icon blocks if the [`Encoding`] supports icons matched in the [`icon::IconTheme`]
    #[default]
    Auto,
    /// Always print icon blocks if included in configured blocks
    Always,
    /// Never print icon blocks
    Never,
}

impl std::fmt::Display for IconWhen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl IconWhen {
    fn retain_ref<B: BlockEnum, T>(
        &self,
        devices: &[&T],
        blocks: &mut Vec<impl Block<B, T>>,
        settings: &PrintSettings,
    ) {
        match self {
            IconWhen::Never => {
                blocks.retain(|b| !b.is_icon());
            }
            IconWhen::Auto => {
                let valid_icons = devices
                    .iter()
                    // all must be valid to avoid tofu chars
                    .all(|d| has_valid_icons(*d, blocks, settings));
                if settings.icons.is_none() || !valid_icons {
                    log::debug!("{:?} removing icon blocks", settings.icon_when);
                    blocks.retain(|b| !b.is_icon());
                }
            }
            IconWhen::Always => {
                if settings.icons.is_none() {
                    log::warn!(
                        "{:?} blocks requested but no icons provided",
                        settings.icon_when
                    );
                }
            }
        }
    }

    fn retain<B: BlockEnum, T>(
        &self,
        devices: &[T],
        blocks: &mut Vec<impl Block<B, T>>,
        settings: &PrintSettings,
    ) {
        match self {
            IconWhen::Never => {
                blocks.retain(|b| !b.is_icon());
            }
            IconWhen::Auto => {
                let valid_icons = devices
                    .iter()
                    // all must be valid to avoid tofu chars
                    .all(|d| has_valid_icons(d, blocks, settings));
                if settings.icons.is_none() || !valid_icons {
                    log::debug!("{:?} removing icon blocks", settings.icon_when);
                    blocks.retain(|b| !b.is_icon());
                }
            }
            IconWhen::Always => {
                if settings.icons.is_none() {
                    log::warn!(
                        "{:?} blocks requested but no icons provided",
                        settings.icon_when
                    );
                }
            }
        }
    }
}

/// Character encoding control for the output
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Encoding {
    /// Use UTF-8 private use area characters such as those used by NerdFont to show glyph icons
    #[default]
    Glyphs,
    /// Use only standard UTF-8 characters for the output; no private use area glyph icons
    Utf8,
    /// Use only ASCII characters for the output; 0x00 - 0x7F (127 chars)
    Ascii,
}

impl std::fmt::Display for Encoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Encoding {
    /// Returns if a char is valid for the encoding for not
    ///
    /// ```
    /// use cyme::display::Encoding;
    ///
    /// let enc = Encoding::Ascii;
    /// assert!(enc.char_is_valid('I'));
    /// assert!(!enc.char_is_valid('\u{2000}'));
    /// assert!(!enc.char_is_valid('●'));
    ///
    /// let enc = Encoding::Utf8;
    /// assert!(enc.char_is_valid('I'));
    /// assert!(enc.char_is_valid('\u{2000}'));
    /// assert!(enc.char_is_valid('●'));
    /// assert!(!enc.char_is_valid('\u{e001}'));
    /// assert!(!enc.char_is_valid(''));
    ///
    /// let enc = Encoding::Glyphs;
    /// assert!(enc.char_is_valid('I'));
    /// assert!(enc.char_is_valid('\u{2000}'));
    /// assert!(enc.char_is_valid('\u{f287}'));
    /// assert!(enc.char_is_valid('\u{e001}'));
    /// assert!(enc.char_is_valid(''));
    /// ```
    pub fn char_is_valid(&self, c: char) -> bool {
        match self {
            Encoding::Ascii if !c.is_ascii() => false,
            // not inside private use area
            Encoding::Utf8 => !matches!(c,
                '\u{E000}'..='\u{F8FF}' |
                '\u{F0000}'..='\u{FFFFD}' |
                '\u{100000}'..='\u{10FFFD}'),
            _ => true,
        }
    }

    /// Returns if a str is valid for the encoding for not
    ///
    /// ```
    /// use cyme::display::Encoding;
    ///
    /// let enc = Encoding::Ascii;
    /// assert!(enc.str_is_valid("hello world"));
    /// assert!(!enc.str_is_valid("├──")); // utf-8 tree
    /// assert!(!enc.str_is_valid("chip "));
    ///
    /// let enc = Encoding::Utf8;
    /// assert!(enc.str_is_valid("hello world"));
    /// assert!(enc.str_is_valid("├──")); // utf-8 tree
    /// assert!(!enc.str_is_valid("chip "));
    ///
    /// let enc = Encoding::Glyphs;
    /// assert!(enc.str_is_valid("hello world"));
    /// assert!(enc.str_is_valid("├──")); // utf-8 tree
    /// assert!(enc.str_is_valid("chip "));
    /// ```
    pub fn str_is_valid(&self, s: &str) -> bool {
        s.chars().all(|c| self.char_is_valid(c))
    }
}

/// Info that can be printed about a [`Device`]
#[non_exhaustive]
#[derive(
    Debug,
    EnumIter,
    VariantArray,
    ValueEnum,
    Display,
    Copy,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Clone,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceBlocks {
    /// Number of bus device is attached
    BusNumber,
    /// Bus issued device number
    DeviceNumber,
    /// Position of device in parent branch
    BranchPosition,
    /// Linux style port path
    PortPath,
    /// Linux udev reported syspath
    SysPath,
    /// Linux udev reported driver loaded for device
    Driver,
    /// Icon based on VID/PID
    Icon,
    /// Unique vendor identifier - purchased from USB IF
    VendorId,
    /// Vendor unique product identifier
    ProductId,
    /// Unique vendor identifier and product identifier as a string formatted "vid:pid" like lsusb
    VidPid,
    /// The device name as reported in descriptor or using usb_ids if None
    Name,
    /// The device manufacturer as provided in descriptor or using usb_ids if None
    Manufacturer,
    /// The device product name as reported by usb_ids vidpid lookup
    ProductName,
    /// The device vendor name as reported by usb_ids vid lookup
    VendorName,
    /// Device serial string as reported by descriptor
    Serial,
    /// Advertised device capable speed
    Speed,
    /// Negotiated device speed as connected
    NegotiatedSpeed,
    /// Position along all branches back to trunk device
    TreePositions,
    /// macOS system_profiler only - actually bus current in mA not power!
    BusPower,
    /// macOS system_profiler only - actually bus current used in mA not power!
    BusPowerUsed,
    /// macOS system_profiler only - actually bus current used in mA not power!
    ExtraCurrentUsed,
    /// The device version
    BcdDevice,
    /// The supported USB version
    BcdUsb,
    /// Base class enum of interface provided by USB IF - only available when using libusb
    #[serde(alias = "class-code")] // was called ClassCode in previous versions
    BaseClass,
    /// Sub-class value of interface provided by USB IF - only available when using libusb
    SubClass,
    /// Prototol value for interface provided by USB IF - only available when using libusb
    Protocol,
    /// Class name from USB IDs repository
    UidClass,
    /// Sub-class name from USB IDs repository
    UidSubClass,
    /// Protocol name from USB IDs repository
    UidProtocol,
    /// Fully defined USB Class Code enum based on BaseClass/SubClass/Protocol triplet
    Class,
    /// Base class as number value rather than enum
    #[serde(alias = "class-value")] // was called ClassCode in previous versions
    BaseValue,
    /// Last time device was seen
    LastEvent,
    /// Event icon
    EventIcon,
}

/// Info that can be printed about a [`Bus`]
#[non_exhaustive]
#[derive(
    Debug,
    Copy,
    EnumIter,
    VariantArray,
    ValueEnum,
    Display,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Clone,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum BusBlocks {
    /// System bus number identifier
    BusNumber,
    /// Icon based on VID/PID
    Icon,
    /// System internal bus name based on Root Hub device name
    Name,
    /// System internal bus provider name
    HostController,
    /// Vendor name of PCI Host Controller from pci.ids
    HostControllerVendor,
    /// Device name of PCI Host Controller from pci.ids
    HostControllerDevice,
    /// PCI vendor ID (VID)
    PciVendor,
    /// PCI device ID (PID)
    PciDevice,
    /// PCI Revsision ID
    PciRevision,
    /// syspath style port path to bus, applicable to Linux only
    PortPath,
}

/// Info that can be printed about a [`Configuration`]
#[non_exhaustive]
#[derive(
    Debug,
    Copy,
    EnumIter,
    VariantArray,
    ValueEnum,
    Display,
    Eq,
    PartialEq,
    Hash,
    Clone,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigurationBlocks {
    /// Name from string descriptor
    Name,
    /// Number of config, bConfigurationValue; value to set to enable to configuration
    Number,
    /// Interfaces available for this configuruation
    NumInterfaces,
    /// Attributes of configuration, bmAttributes
    Attributes,
    /// Icon representation of bmAttributes
    IconAttributes,
    /// Maximum current consumption in mA
    MaxPower,
}

/// Info that can be printed about a [`Interface`]
#[non_exhaustive]
#[derive(
    Debug,
    Copy,
    EnumIter,
    VariantArray,
    ValueEnum,
    Display,
    Eq,
    PartialEq,
    Hash,
    Clone,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum InterfaceBlocks {
    /// Name from string descriptor
    Name,
    /// Interface number
    Number,
    /// Interface port path, applicable to Linux
    PortPath,
    /// Base class enum of interface provided by USB IF
    #[serde(alias = "class-code")] // was called ClassCode in previous versions
    BaseClass,
    /// Sub-class value of interface provided by USB IF
    SubClass,
    /// Prototol value for interface provided by USB IF
    Protocol,
    /// Interfaces can have the same number but an alternate settings defined here
    AltSetting,
    /// Driver obtained from udev on Linux only
    Driver,
    /// syspath obtained from udev on Linux only
    SysPath,
    /// An interface can have many endpoints
    NumEndpoints,
    /// Icon based on BaseClass/SubCode/Protocol
    Icon,
    /// Class name from USB IDs repository
    UidClass,
    /// Sub-class name from USB IDs repository
    UidSubClass,
    /// Protocol name from USB IDs repository
    UidProtocol,
    /// Fully defined USB Class Code based on BaseClass/SubClass/Protocol triplet
    Class,
    /// Base class as number value rather than enum
    #[serde(alias = "class-value")]
    BaseValue,
}

/// Info that can be printed about a [`Endpoint`]
#[non_exhaustive]
#[derive(
    Debug,
    Copy,
    EnumIter,
    VariantArray,
    ValueEnum,
    Display,
    Eq,
    PartialEq,
    Hash,
    Clone,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum EndpointBlocks {
    /// Endpoint number on interface
    Number,
    /// Direction of data into endpoint
    Direction,
    /// Type of data transfer endpoint accepts
    TransferType,
    /// Synchronisation type (Iso mode)
    SyncType,
    /// Usage type (Iso mode)
    UsageType,
    /// Maximum packet size in bytes endpoint can send/recieve
    MaxPacketSize,
    /// Interval for polling endpoint data transfers. Value in frame counts. Ignored for Bulk & Control Endpoints. Isochronous must equal 1 and field may range from 1 to 255 for interrupt endpoints.
    Interval,
}

/// Length of field printed by block
#[derive(Debug, Eq, PartialEq)]
pub enum BlockLength {
    /// Fixed length like numbers with padding
    Fixed(usize),
    /// Variable length such as string descriptors - contained value is the heading (min) length
    Variable(usize),
}

impl BlockLength {
    /// Get the length contained in Enum
    pub fn len(self) -> usize {
        match self {
            BlockLength::Fixed(s) => s,
            BlockLength::Variable(s) => s,
        }
    }

    /// Is the length zero
    pub fn is_empty(self) -> bool {
        match self {
            BlockLength::Fixed(s) => s == 0,
            BlockLength::Variable(s) => s == 0,
        }
    }

    /// Get the fixed length if `[BlockLength::Fixed]` else None
    pub fn fixed_len(self) -> Option<usize> {
        match self {
            BlockLength::Fixed(s) => Some(s),
            _ => None,
        }
    }

    /// Get the variable length if `[BlockLength::Variable]` else None
    pub fn variable_len(self) -> Option<usize> {
        match self {
            BlockLength::Variable(s) => Some(s),
            _ => None,
        }
    }
}

/// Helper trait to allow for generic block handling
pub trait BlockEnum: Eq + Hash + VariantArray + ValueEnum {}
impl BlockEnum for DeviceBlocks {}
impl BlockEnum for BusBlocks {}
impl BlockEnum for ConfigurationBlocks {}
impl BlockEnum for InterfaceBlocks {}
impl BlockEnum for EndpointBlocks {}

/// Intended to be `impl` by a xxxBlocks `enum`
pub trait Block<B: BlockEnum, T> {
    /// The inset when printing non-tree as a list
    const INSET: u8 = 0;

    /// List of default blocks to use for printing T with optional `verbose` for maximum verbosity
    fn default_blocks(verbose: bool) -> Vec<Self>
    where
        Self: Sized;

    /// Example blocks for generated files
    fn example_blocks() -> Vec<Self>
    where
        Self: Sized,
    {
        Self::default_blocks(false)
    }

    /// Returns the length of block value given device data - like block_length but actual device field length rather than fixed/heading
    fn len(&self, d: &[&T]) -> usize;

    /// Returns length type and usize contained, [`BlockLength::Variable`] will be heading usize without actual device data
    fn block_length(&self) -> BlockLength;

    /// Creates a HashMap of B keys to usize of longest value for that key in the `d` Vec or heading if > this; values can then be padded to match this
    fn generate_padding(d: &[&T]) -> HashMap<B, usize>;

    /// Colour the block String
    fn colour(&self, s: &str, ct: &colour::ColourTheme) -> ColoredString;

    /// Creates the heading for the block value, for use with the heading flag
    fn heading(&self) -> &str;

    /// Pads the heading with provided padding block HashMap
    fn heading_padded(&self, pad: &HashMap<B, usize>) -> String;

    /// Returns whether the value intended for the block is a variable length type (string descriptor)
    fn value_is_variable_length(&self) -> bool {
        match self.block_length() {
            BlockLength::Fixed(_) => false,
            BlockLength::Variable(_) => true,
        }
    }

    /// Formats the value associated with the block into a display String
    fn format_value(
        &self,
        d: &T,
        pad: &HashMap<B, usize>,
        settings: &PrintSettings,
    ) -> Option<String>;

    /// Formats u16 values like VID as base16 or base10 depending on decimal setting
    fn format_base_u16(v: u16, settings: &PrintSettings) -> String {
        if settings.decimal {
            // pad 6 not 5 to maintian 0x padding
            format!("{v:6}")
        } else {
            format!("0x{v:04x}")
        }
    }

    /// Formats u8 values like codes as base16 or base10 depending on decimal setting
    fn format_base_u8(v: u8, settings: &PrintSettings) -> String {
        if settings.decimal {
            format!("{v:4}")
        } else {
            format!("0x{v:02x}")
        }
    }

    /// Formats VID and PID values into a string like "vid:pid" with padding
    fn format_vidpid(v: Option<u16>, p: Option<u16>, settings: &PrintSettings) -> String {
        match (v, p) {
            (Some(v), Some(p)) => {
                if settings.decimal {
                    format!("{v:>5}:{p:<5}")
                } else {
                    format!(" {v:04x}:{p:04x} ")
                }
            }
            _ => format!("{:>5}:{:<5}", "-", "-"),
        }
    }

    /// If the block is used for icons
    fn is_icon(&self) -> bool {
        false
    }

    /// Get static array of all blocks for this type
    fn all_blocks() -> &'static [B]
    where
        Self: Sized,
    {
        B::VARIANTS
    }
}

impl DeviceBlocks {
    /// Default `DeviceBlocks` for watch mode printing
    pub fn default_watch_blocks(verbose: bool, tree: bool) -> Vec<Self> {
        let mut blocks = if tree {
            Self::default_device_tree_blocks()
        } else {
            Self::default_blocks(verbose)
        };
        blocks.push(DeviceBlocks::EventIcon);
        blocks.push(DeviceBlocks::LastEvent);
        blocks
    }

    /// Default `DeviceBlocks` for tree printing are different to list, get them here
    pub fn default_device_tree_blocks() -> Vec<Self> {
        #[cfg(target_os = "linux")]
        {
            vec![
                DeviceBlocks::Icon,
                DeviceBlocks::BranchPosition,
                DeviceBlocks::DeviceNumber,
                DeviceBlocks::VendorId,
                DeviceBlocks::ProductId,
                DeviceBlocks::Name,
                DeviceBlocks::Serial,
                DeviceBlocks::Driver,
            ]
        }

        #[cfg(not(target_os = "linux"))]
        {
            vec![
                DeviceBlocks::Icon,
                DeviceBlocks::BranchPosition,
                DeviceBlocks::DeviceNumber,
                DeviceBlocks::VendorId,
                DeviceBlocks::ProductId,
                DeviceBlocks::Name,
                DeviceBlocks::Serial,
            ]
        }
    }
}

impl Block<DeviceBlocks, Device> for DeviceBlocks {
    #[cfg(target_os = "linux")]
    fn default_blocks(verbose: bool) -> Vec<Self> {
        if verbose {
            vec![
                DeviceBlocks::BusNumber,
                DeviceBlocks::DeviceNumber,
                DeviceBlocks::TreePositions,
                DeviceBlocks::PortPath,
                DeviceBlocks::Icon,
                DeviceBlocks::VendorId,
                DeviceBlocks::ProductId,
                DeviceBlocks::BcdDevice,
                DeviceBlocks::BcdUsb,
                DeviceBlocks::BaseValue,
                DeviceBlocks::BaseClass,
                DeviceBlocks::SubClass,
                DeviceBlocks::UidSubClass,
                DeviceBlocks::Protocol,
                DeviceBlocks::UidProtocol,
                DeviceBlocks::Name,
                DeviceBlocks::Manufacturer,
                DeviceBlocks::Serial,
                DeviceBlocks::Driver,
                DeviceBlocks::SysPath,
                DeviceBlocks::Speed,
            ]
        } else {
            vec![
                DeviceBlocks::BusNumber,
                DeviceBlocks::DeviceNumber,
                DeviceBlocks::Icon,
                DeviceBlocks::VendorId,
                DeviceBlocks::ProductId,
                DeviceBlocks::Name,
                DeviceBlocks::Serial,
                DeviceBlocks::Driver,
                DeviceBlocks::Speed,
            ]
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn default_blocks(verbose: bool) -> Vec<Self> {
        if verbose {
            vec![
                DeviceBlocks::BusNumber,
                DeviceBlocks::DeviceNumber,
                DeviceBlocks::TreePositions,
                DeviceBlocks::PortPath,
                DeviceBlocks::Icon,
                DeviceBlocks::VendorId,
                DeviceBlocks::ProductId,
                DeviceBlocks::BcdDevice,
                DeviceBlocks::BcdUsb,
                DeviceBlocks::BaseValue,
                DeviceBlocks::BaseClass,
                DeviceBlocks::SubClass,
                DeviceBlocks::UidSubClass,
                DeviceBlocks::Protocol,
                DeviceBlocks::UidProtocol,
                DeviceBlocks::Name,
                DeviceBlocks::Manufacturer,
                DeviceBlocks::Serial,
                DeviceBlocks::Speed,
            ]
        } else {
            vec![
                DeviceBlocks::BusNumber,
                DeviceBlocks::DeviceNumber,
                DeviceBlocks::Icon,
                DeviceBlocks::VendorId,
                DeviceBlocks::ProductId,
                DeviceBlocks::Name,
                DeviceBlocks::Serial,
                DeviceBlocks::Speed,
            ]
        }
    }

    fn example_blocks() -> Vec<Self> {
        vec![
            DeviceBlocks::BusNumber,
            DeviceBlocks::DeviceNumber,
            DeviceBlocks::Icon,
            DeviceBlocks::VendorId,
            DeviceBlocks::ProductId,
            DeviceBlocks::Name,
            DeviceBlocks::Serial,
            DeviceBlocks::Driver,
            DeviceBlocks::Speed,
        ]
    }

    fn len(&self, d: &[&Device]) -> usize {
        match self {
            DeviceBlocks::Name => d.iter().map(|d| d.name.width()).max().unwrap_or(0),
            DeviceBlocks::Serial => d
                .iter()
                .flat_map(|d| d.serial_num.as_ref().map(|s| s.width()))
                .max()
                .unwrap_or(0),
            DeviceBlocks::Manufacturer => d
                .iter()
                .flat_map(|d| d.manufacturer.as_ref().map(|s| s.width()))
                .max()
                .unwrap_or(0),
            DeviceBlocks::TreePositions => d
                .iter()
                .map(|d| d.location_id.tree_positions.len() * 2)
                .max()
                .unwrap_or(0),
            DeviceBlocks::PortPath => d
                .iter()
                // byte len ok as I know it's all ascii
                .map(|d| d.port_path().to_string().len())
                .max()
                .unwrap_or(0),
            DeviceBlocks::SysPath => d
                .iter()
                .flat_map(|d| {
                    d.extra
                        .as_ref()
                        .and_then(|e| e.syspath.as_ref().map(|s| s.len()))
                })
                .max()
                .unwrap_or(0),
            DeviceBlocks::Driver => d
                .iter()
                .flat_map(|d| {
                    d.extra
                        .as_ref()
                        .and_then(|e| e.driver.as_ref().map(|s| s.len()))
                })
                .max()
                .unwrap_or(0),
            DeviceBlocks::ProductName => d
                .iter()
                .flat_map(|d| {
                    d.extra
                        .as_ref()
                        .and_then(|e| e.product_name.as_ref().map(|s| s.width()))
                })
                .max()
                .unwrap_or(0),
            DeviceBlocks::VendorName => d
                .iter()
                .flat_map(|d| {
                    d.extra
                        .as_ref()
                        .and_then(|e| e.vendor.as_ref().map(|s| s.width()))
                })
                .max()
                .unwrap_or(0),
            DeviceBlocks::BaseClass => d
                .iter()
                .flat_map(|d| d.class.as_ref().map(|c| c.to_string().len()))
                .max()
                .unwrap_or(0),
            DeviceBlocks::UidClass => d
                .iter()
                .flat_map(|d| d.class_name().map(|s| s.len()))
                .max()
                .unwrap_or(0),
            DeviceBlocks::UidSubClass => d
                .iter()
                .flat_map(|d| d.sub_class_name().map(|s| s.len()))
                .max()
                .unwrap_or(0),
            DeviceBlocks::UidProtocol => d
                .iter()
                .flat_map(|d| d.protocol_name().map(|s| s.len()))
                .max()
                .unwrap_or(0),
            DeviceBlocks::Class => d
                .iter()
                .map(|d| d.fully_defined_class().map_or(0, |c| c.to_string().len()))
                .max()
                .unwrap_or(0),
            DeviceBlocks::LastEvent => d
                .iter()
                .flat_map(|d| d.last_event().map(|s| s.to_string().len()))
                .max()
                .unwrap_or(0),
            _ => self.block_length().len(),
        }
    }

    fn generate_padding(d: &[&Device]) -> HashMap<Self, usize> {
        DeviceBlocks::iter()
            .map(|b| (b, cmp::max(b.heading().len(), b.len(d))))
            .collect()
    }

    fn format_value(
        &self,
        d: &Device,
        pad: &HashMap<Self, usize>,
        settings: &PrintSettings,
    ) -> Option<String> {
        match self {
            DeviceBlocks::BusNumber => Some(format!("{:3}", d.location_id.bus)),
            DeviceBlocks::DeviceNumber => Some(format!("{:3}", d.location_id.number)),
            DeviceBlocks::BranchPosition => Some(format!("{:3}", d.get_branch_position())),
            DeviceBlocks::PortPath => Some(format!(
                "{:pad$}",
                d.port_path().to_string(),
                pad = pad.get(self).unwrap_or(&0)
            )),
            DeviceBlocks::SysPath => Some(match d.extra.as_ref() {
                Some(e) => format!(
                    "{:pad$}",
                    e.syspath.as_ref().unwrap_or(&format!(
                        "{:pad$}",
                        "-",
                        pad = pad.get(self).unwrap_or(&0)
                    )),
                    pad = pad.get(self).unwrap_or(&0)
                ),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::Driver => Some(match d.extra.as_ref() {
                Some(e) => format!(
                    "{:pad$}",
                    e.driver.as_ref().unwrap_or(&format!(
                        "{:pad$}",
                        "-",
                        pad = pad.get(self).unwrap_or(&0)
                    )),
                    pad = pad.get(self).unwrap_or(&0)
                ),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::ProductName => Some(match d.extra.as_ref() {
                Some(e) => format!(
                    "{:pad$}",
                    e.product_name.as_ref().unwrap_or(&format!(
                        "{:pad$}",
                        "-",
                        pad = pad.get(self).unwrap_or(&0)
                    )),
                    pad = pad.get(self).unwrap_or(&0)
                ),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::VendorName => Some(match d.extra.as_ref() {
                Some(e) => format!(
                    "{:pad$}",
                    e.vendor.as_ref().unwrap_or(&format!(
                        "{:pad$}",
                        "-",
                        pad = pad.get(self).unwrap_or(&0)
                    )),
                    pad = pad.get(self).unwrap_or(&0)
                ),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::Icon => settings.icons.as_ref().map(|i| i.get_device_icon(d)),
            DeviceBlocks::VendorId => Some(match d.vendor_id {
                Some(v) => Self::format_base_u16(v, settings),
                None => format!("{:>6}", "-"),
            }),
            DeviceBlocks::ProductId => Some(match d.product_id {
                Some(v) => Self::format_base_u16(v, settings),
                None => format!("{:>6}", "-"),
            }),
            DeviceBlocks::VidPid => Some(Self::format_vidpid(d.vendor_id, d.product_id, settings)),
            DeviceBlocks::Name => Some(format!(
                "{:pad$}",
                d.name,
                pad = pad.get(self).unwrap_or(&0)
            )),
            DeviceBlocks::Manufacturer => Some(match d.manufacturer.as_ref() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::Serial => Some(match d.serial_num.as_ref() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::Speed => Some(match d.device_speed.as_ref() {
                Some(v) => format!("{:>10}", v.to_string()),
                None => format!("{:>10}", "-"),
            }),
            DeviceBlocks::NegotiatedSpeed => Some(
                match d.extra.as_ref().and_then(|e| e.negotiated_speed.as_ref()) {
                    Some(v) => {
                        let nu = NumericalUnit::<f32>::from(v);
                        format!("{:>10}", nu.to_string())
                    }
                    None => format!("{:>10}", "-"),
                },
            ),
            DeviceBlocks::TreePositions => Some(format!(
                "{:pad$}",
                format!("{:}", d.location_id.tree_positions.iter().format("-")),
                pad = pad.get(self).unwrap_or(&0)
            )),
            DeviceBlocks::BusPower => Some(match d.bus_power {
                Some(v) => format!("{v:3} mA"),
                None => format!("{:>6}", "-"),
            }),
            DeviceBlocks::BusPowerUsed => Some(match d.bus_power_used {
                Some(v) => format!("{v:3} mA"),
                None => format!("{:>6}", "-"),
            }),
            DeviceBlocks::ExtraCurrentUsed => Some(match d.extra_current_used {
                Some(v) => format!("{v:3} mA"),
                None => format!("{:>6}", "-"),
            }),
            DeviceBlocks::BcdDevice => Some(match d.bcd_device {
                Some(v) => format!("{:5}", v.to_string()),
                None => format!("{:>5}", "-"),
            }),
            DeviceBlocks::BcdUsb => Some(match d.bcd_usb {
                Some(v) => format!("{:5}", v.to_string()),
                None => format!("{:>5}", "-"),
            }),
            DeviceBlocks::BaseClass => Some(match d.class.as_ref() {
                Some(v) => format!("{:pad$}", v.to_string(), pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::SubClass => Some(match d.sub_class.as_ref() {
                Some(v) => Self::format_base_u8(*v, settings),
                None => format!("{:>4}", "-"),
            }),
            DeviceBlocks::Protocol => Some(match d.protocol.as_ref() {
                Some(v) => Self::format_base_u8(*v, settings),
                None => format!("{:>4}", "-"),
            }),
            DeviceBlocks::UidClass => Some(match d.class_name() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::UidSubClass => Some(match d.sub_class_name() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::UidProtocol => Some(match d.protocol_name() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::Class => Some(match d.fully_defined_class() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::BaseValue => Some(match d.class.as_ref() {
                Some(v) => Self::format_base_u8((*v).into(), settings),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::LastEvent => Some(match d.last_event() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            DeviceBlocks::EventIcon => match d.last_event() {
                Some(e) => settings.icons.as_ref().map(|i| i.get_event_icon(&e)),
                None => None,
            },
        }
    }

    fn colour(&self, s: &str, ct: &colour::ColourTheme) -> ColoredString {
        match self {
            DeviceBlocks::BcdUsb
            | DeviceBlocks::BcdDevice
            | DeviceBlocks::DeviceNumber
            | DeviceBlocks::LastEvent => ct.number.map_or(s.normal(), |c| s.color(c)),
            DeviceBlocks::BusNumber
            | DeviceBlocks::BranchPosition
            | DeviceBlocks::TreePositions => ct.location.map_or(s.normal(), |c| s.color(c)),
            DeviceBlocks::Icon | DeviceBlocks::EventIcon => {
                ct.icon.map_or(s.normal(), |c| s.color(c))
            }
            DeviceBlocks::PortPath | DeviceBlocks::SysPath => {
                ct.path.map_or(s.normal(), |c| s.color(c))
            }
            DeviceBlocks::VendorId | DeviceBlocks::VidPid => {
                ct.vid.map_or(s.normal(), |c| s.color(c))
            }
            DeviceBlocks::ProductId => ct.pid.map_or(s.normal(), |c| s.color(c)),
            DeviceBlocks::Name | DeviceBlocks::ProductName => {
                ct.name.map_or(s.normal(), |c| s.color(c))
            }
            DeviceBlocks::Serial => ct.serial.map_or(s.normal(), |c| s.color(c)),
            DeviceBlocks::Manufacturer | DeviceBlocks::VendorName => {
                ct.manufacturer.map_or(s.normal(), |c| s.color(c))
            }
            DeviceBlocks::Driver => ct.driver.map_or(s.normal(), |c| s.color(c)),
            DeviceBlocks::Speed | DeviceBlocks::NegotiatedSpeed => {
                ct.speed.map_or(s.normal(), |c| s.color(c))
            }
            DeviceBlocks::BusPower
            | DeviceBlocks::BusPowerUsed
            | DeviceBlocks::ExtraCurrentUsed => ct.power.map_or(s.normal(), |c| s.color(c)),
            DeviceBlocks::BaseClass
            | DeviceBlocks::UidClass
            | DeviceBlocks::Class
            | DeviceBlocks::BaseValue => ct.class_code.map_or(s.normal(), |c| s.color(c)),
            DeviceBlocks::SubClass | DeviceBlocks::UidSubClass => {
                ct.sub_code.map_or(s.normal(), |c| s.color(c))
            }
            DeviceBlocks::Protocol | DeviceBlocks::UidProtocol => {
                ct.protocol.map_or(s.normal(), |c| s.color(c))
            }
        }
    }

    fn heading(&self) -> &str {
        match self {
            DeviceBlocks::BusNumber => "Bus",
            DeviceBlocks::DeviceNumber => "#",
            DeviceBlocks::BranchPosition => "Prt",
            DeviceBlocks::PortPath => "PPath",
            DeviceBlocks::SysPath => "SPath",
            DeviceBlocks::Driver => "Driver",
            DeviceBlocks::VendorId => "VID",
            DeviceBlocks::ProductId => "PID",
            DeviceBlocks::VidPid => "VID:PID",
            DeviceBlocks::Name => "Name",
            DeviceBlocks::Manufacturer => "Manfacturer",
            DeviceBlocks::ProductName => "PName",
            DeviceBlocks::VendorName => "VName",
            DeviceBlocks::Serial => "Serial",
            DeviceBlocks::Speed => "Speed",
            DeviceBlocks::NegotiatedSpeed => "NgSpd",
            DeviceBlocks::TreePositions => "TPos",
            // will be 000 mA = 6
            DeviceBlocks::BusPower => "PBus",
            DeviceBlocks::BusPowerUsed => "PUsd",
            DeviceBlocks::ExtraCurrentUsed => "PExr",
            // 00.00 = 5
            DeviceBlocks::BcdDevice => "Dev V",
            DeviceBlocks::BcdUsb => "USB V",
            DeviceBlocks::BaseClass => "BaseC",
            DeviceBlocks::SubClass => "SubC",
            DeviceBlocks::Protocol => "Pcol",
            DeviceBlocks::UidClass => "UidCl",
            DeviceBlocks::UidSubClass => "UidSc",
            DeviceBlocks::UidProtocol => "UidPc",
            DeviceBlocks::Class => "Class",
            DeviceBlocks::BaseValue => "CVal",
            DeviceBlocks::Icon => ICON_HEADING,
            DeviceBlocks::EventIcon => "E",
            DeviceBlocks::LastEvent => "Event",
        }
    }

    fn heading_padded(&self, pad: &HashMap<Self, usize>) -> String {
        format!(
            "{:^pad$}",
            self.heading(),
            pad = pad.get(self).unwrap_or(&0)
        )
    }

    fn block_length(&self) -> BlockLength {
        match self {
            DeviceBlocks::Icon | DeviceBlocks::EventIcon => BlockLength::Fixed(1),
            DeviceBlocks::BusNumber | DeviceBlocks::DeviceNumber | DeviceBlocks::BranchPosition => {
                BlockLength::Fixed(3)
            }
            DeviceBlocks::VendorId | DeviceBlocks::ProductId => BlockLength::Fixed(6),
            DeviceBlocks::VidPid => BlockLength::Fixed(11),
            DeviceBlocks::Speed => BlockLength::Fixed(10),
            DeviceBlocks::NegotiatedSpeed => BlockLength::Fixed(10),
            DeviceBlocks::BusPower
            | DeviceBlocks::BusPowerUsed
            | DeviceBlocks::ExtraCurrentUsed => BlockLength::Fixed(6),
            DeviceBlocks::BcdDevice | DeviceBlocks::BcdUsb => BlockLength::Fixed(5),
            DeviceBlocks::SubClass | DeviceBlocks::Protocol | DeviceBlocks::BaseValue => {
                BlockLength::Fixed(4)
            }
            _ => BlockLength::Variable(self.heading().len()),
        }
    }

    fn is_icon(&self) -> bool {
        self == &DeviceBlocks::Icon
    }
}

impl Block<BusBlocks, Bus> for BusBlocks {
    fn default_blocks(verbose: bool) -> Vec<Self> {
        if verbose {
            vec![
                BusBlocks::Icon,
                BusBlocks::PortPath,
                BusBlocks::Name,
                BusBlocks::HostController,
                BusBlocks::HostControllerDevice,
                BusBlocks::HostControllerVendor,
                BusBlocks::PciVendor,
                BusBlocks::PciDevice,
                BusBlocks::PciRevision,
            ]
        } else {
            vec![
                BusBlocks::PortPath,
                BusBlocks::Name,
                BusBlocks::HostController,
                BusBlocks::HostControllerDevice,
            ]
        }
    }

    fn len(&self, d: &[&Bus]) -> usize {
        match self {
            BusBlocks::Name => d.iter().map(|d| d.name.width()).max().unwrap_or(0),
            BusBlocks::HostController => d
                .iter()
                .map(|d| d.host_controller.width())
                .max()
                .unwrap_or(0),
            BusBlocks::HostControllerVendor => d
                .iter()
                .flat_map(|d| d.host_controller_vendor.as_ref().map(|v| v.width()))
                .max()
                .unwrap_or(0),
            BusBlocks::HostControllerDevice => d
                .iter()
                .flat_map(|d| d.host_controller_device.as_ref().map(|v| v.width()))
                .max()
                .unwrap_or(0),
            BusBlocks::PortPath => d
                .iter()
                .map(|d| d.path().unwrap_or_default().as_os_str().len())
                .max()
                .unwrap_or(0),
            _ => self.block_length().len(),
        }
    }

    fn generate_padding(d: &[&Bus]) -> HashMap<Self, usize> {
        BusBlocks::iter()
            .map(|b| (b, cmp::max(b.heading().len(), b.len(d))))
            .collect()
    }

    fn colour(&self, s: &str, ct: &colour::ColourTheme) -> ColoredString {
        match self {
            BusBlocks::BusNumber => ct.location.map_or(s.normal(), |c| s.color(c)),
            BusBlocks::PciVendor => ct.vid.map_or(s.normal(), |c| s.color(c)),
            BusBlocks::PciDevice => ct.pid.map_or(s.normal(), |c| s.color(c)),
            BusBlocks::Name => ct.name.map_or(s.normal(), |c| s.color(c)),
            BusBlocks::HostController => ct.class_code.map_or(s.normal(), |c| s.color(c)),
            BusBlocks::HostControllerVendor => ct.manufacturer.map_or(s.normal(), |c| s.color(c)),
            BusBlocks::HostControllerDevice => ct.name.map_or(s.normal(), |c| s.color(c)),
            BusBlocks::PciRevision => ct.number.map_or(s.normal(), |c| s.color(c)),
            BusBlocks::Icon => ct.icon.map_or(s.normal(), |c| s.color(c)),
            BusBlocks::PortPath => ct.path.map_or(s.normal(), |c| s.color(c)),
        }
    }

    fn format_value(
        &self,
        bus: &Bus,
        pad: &HashMap<Self, usize>,
        settings: &PrintSettings,
    ) -> Option<String> {
        match self {
            BusBlocks::BusNumber => bus
                .get_bus_number()
                .map(|v| format!("{v:3}"))
                .or(Some("---".to_string())),
            BusBlocks::Icon => settings
                .icons
                .as_ref()
                .map(|i| i.get_bus_icon(bus))
                .or(Some(" ".to_string())),
            BusBlocks::PciVendor => Some(match bus.pci_vendor {
                Some(v) => Self::format_base_u16(v, settings),
                None => format!("{:>6}", "-"),
            }),
            BusBlocks::PciDevice => Some(match bus.pci_device {
                Some(v) => Self::format_base_u16(v, settings),
                None => format!("{:>6}", "-"),
            }),
            BusBlocks::PciRevision => Some(match bus.pci_revision {
                Some(v) => Self::format_base_u16(v, settings),
                None => format!("{:>6}", "-"),
            }),
            BusBlocks::Name => Some(format!(
                "{:pad$}",
                bus.name,
                pad = pad.get(self).unwrap_or(&0)
            )),
            BusBlocks::HostController => Some(format!(
                "{:pad$}",
                bus.host_controller,
                pad = pad.get(self).unwrap_or(&0)
            )),
            BusBlocks::HostControllerVendor => Some(match bus.host_controller_vendor.as_ref() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            BusBlocks::HostControllerDevice => Some(match bus.host_controller_device.as_ref() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            BusBlocks::PortPath => Some(match bus.path() {
                Some(v) => format!("{:pad$}", v.display(), pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
        }
    }

    fn heading(&self) -> &str {
        match self {
            BusBlocks::BusNumber => "Bus",
            BusBlocks::PortPath => "PPath",
            BusBlocks::PciDevice => "VID",
            BusBlocks::PciVendor => "PID",
            BusBlocks::PciRevision => "Revisn",
            BusBlocks::Name => "Name",
            BusBlocks::HostController => "HostController",
            BusBlocks::HostControllerVendor => "HostVendor",
            BusBlocks::HostControllerDevice => "HostDevice",
            BusBlocks::Icon => ICON_HEADING,
        }
    }

    fn heading_padded(&self, pad: &HashMap<Self, usize>) -> String {
        format!(
            "{:^pad$}",
            self.heading(),
            pad = pad.get(self).unwrap_or(&0)
        )
    }

    fn block_length(&self) -> BlockLength {
        match self {
            BusBlocks::Icon => BlockLength::Fixed(1),
            BusBlocks::BusNumber => BlockLength::Fixed(3),
            BusBlocks::PciDevice | BusBlocks::PciVendor | BusBlocks::PciRevision => {
                BlockLength::Fixed(6)
            }
            _ => BlockLength::Variable(self.heading().len()),
        }
    }

    fn is_icon(&self) -> bool {
        self == &BusBlocks::Icon
    }
}

impl Block<ConfigurationBlocks, Configuration> for ConfigurationBlocks {
    const INSET: u8 = 1;

    fn default_blocks(verbose: bool) -> Vec<Self> {
        if verbose {
            vec![
                ConfigurationBlocks::Number,
                ConfigurationBlocks::IconAttributes,
                ConfigurationBlocks::Attributes,
                ConfigurationBlocks::NumInterfaces,
                ConfigurationBlocks::MaxPower,
                ConfigurationBlocks::Name,
            ]
        } else {
            vec![
                ConfigurationBlocks::Number,
                ConfigurationBlocks::IconAttributes,
                ConfigurationBlocks::MaxPower,
                ConfigurationBlocks::Name,
            ]
        }
    }

    fn len(&self, d: &[&Configuration]) -> usize {
        match self {
            ConfigurationBlocks::Name => d.iter().map(|d| d.name.len()).max().unwrap_or(0),
            ConfigurationBlocks::Attributes => d
                .iter()
                .map(|d| d.attributes_string().len())
                .max()
                .unwrap_or(0),
            _ => self.block_length().len(),
        }
    }

    fn generate_padding(d: &[&Configuration]) -> HashMap<Self, usize> {
        ConfigurationBlocks::iter()
            .map(|b| (b, cmp::max(b.heading().len(), b.len(d))))
            .collect()
    }

    fn colour(&self, s: &str, ct: &colour::ColourTheme) -> ColoredString {
        match self {
            ConfigurationBlocks::Number => ct.location.map_or(s.normal(), |c| s.color(c)),
            ConfigurationBlocks::NumInterfaces => ct.number.map_or(s.normal(), |c| s.color(c)),
            ConfigurationBlocks::MaxPower => ct.power.map_or(s.normal(), |c| s.color(c)),
            ConfigurationBlocks::Name => ct.name.map_or(s.normal(), |c| s.color(c)),
            ConfigurationBlocks::Attributes => ct.attributes.map_or(s.normal(), |c| s.color(c)),
            ConfigurationBlocks::IconAttributes => ct.icon.map_or(s.normal(), |c| s.color(c)),
        }
    }

    fn format_value(
        &self,
        config: &Configuration,
        pad: &HashMap<Self, usize>,
        settings: &PrintSettings,
    ) -> Option<String> {
        match self {
            ConfigurationBlocks::Number => Some(format!("{:2}", config.number)),
            ConfigurationBlocks::NumInterfaces => Some(format!("{:2}", config.interfaces.len())),
            ConfigurationBlocks::Name => Some(format!(
                "{:pad$}",
                config.name,
                pad = pad.get(self).unwrap_or(&0)
            )),
            ConfigurationBlocks::MaxPower => Some(format!("{:6}", config.max_power)),
            ConfigurationBlocks::Attributes => Some(format!(
                "{:pad$}",
                config.attributes_string(),
                pad = pad.get(self).unwrap_or(&0)
            )),
            ConfigurationBlocks::IconAttributes => Some(format!(
                "{:pad$}",
                attributes_to_icons(&config.attributes, settings),
                pad = pad.get(self).unwrap_or(&0)
            )),
        }
    }

    fn heading(&self) -> &str {
        match self {
            ConfigurationBlocks::Number => "#",
            ConfigurationBlocks::NumInterfaces => "I#",
            ConfigurationBlocks::MaxPower => "PMax",
            ConfigurationBlocks::Name => "Name",
            ConfigurationBlocks::Attributes => "Attributes",
            ConfigurationBlocks::IconAttributes => ICON_HEADING,
        }
    }

    fn heading_padded(&self, pad: &HashMap<Self, usize>) -> String {
        format!(
            "{:^pad$}",
            self.heading(),
            pad = pad.get(self).unwrap_or(&0)
        )
    }

    fn block_length(&self) -> BlockLength {
        match self {
            ConfigurationBlocks::Number => BlockLength::Fixed(2),
            ConfigurationBlocks::NumInterfaces => BlockLength::Fixed(2),
            ConfigurationBlocks::MaxPower => BlockLength::Fixed(6),
            // two possible icons and a space between
            ConfigurationBlocks::IconAttributes => BlockLength::Fixed(3),
            _ => BlockLength::Variable(self.heading().len()),
        }
    }

    fn is_icon(&self) -> bool {
        self == &ConfigurationBlocks::IconAttributes
    }
}

impl Block<InterfaceBlocks, Interface> for InterfaceBlocks {
    const INSET: u8 = 2;

    #[cfg(target_os = "linux")]
    fn default_blocks(verbose: bool) -> Vec<Self> {
        if verbose {
            vec![
                InterfaceBlocks::PortPath,
                InterfaceBlocks::Icon,
                InterfaceBlocks::AltSetting,
                InterfaceBlocks::BaseValue,
                InterfaceBlocks::BaseClass,
                InterfaceBlocks::SubClass,
                InterfaceBlocks::UidSubClass,
                InterfaceBlocks::Protocol,
                InterfaceBlocks::UidProtocol,
                InterfaceBlocks::Name,
                InterfaceBlocks::NumEndpoints,
                InterfaceBlocks::Driver,
                InterfaceBlocks::SysPath,
            ]
        } else {
            vec![
                InterfaceBlocks::PortPath,
                InterfaceBlocks::Icon,
                InterfaceBlocks::AltSetting,
                InterfaceBlocks::BaseClass,
                InterfaceBlocks::SubClass,
                InterfaceBlocks::Protocol,
                InterfaceBlocks::Name,
                InterfaceBlocks::Driver,
            ]
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn default_blocks(verbose: bool) -> Vec<Self> {
        if verbose {
            vec![
                InterfaceBlocks::PortPath,
                InterfaceBlocks::Icon,
                InterfaceBlocks::AltSetting,
                InterfaceBlocks::BaseValue,
                InterfaceBlocks::BaseClass,
                InterfaceBlocks::SubClass,
                InterfaceBlocks::UidSubClass,
                InterfaceBlocks::Protocol,
                InterfaceBlocks::UidProtocol,
                InterfaceBlocks::Name,
                InterfaceBlocks::NumEndpoints,
            ]
        } else {
            vec![
                InterfaceBlocks::PortPath,
                InterfaceBlocks::Icon,
                InterfaceBlocks::AltSetting,
                InterfaceBlocks::BaseClass,
                InterfaceBlocks::SubClass,
                InterfaceBlocks::Protocol,
                InterfaceBlocks::Name,
            ]
        }
    }

    fn example_blocks() -> Vec<Self> {
        vec![
            InterfaceBlocks::PortPath,
            InterfaceBlocks::Icon,
            InterfaceBlocks::AltSetting,
            InterfaceBlocks::BaseClass,
            InterfaceBlocks::SubClass,
            InterfaceBlocks::Protocol,
            InterfaceBlocks::Name,
            InterfaceBlocks::Driver,
        ]
    }

    fn len(&self, d: &[&Interface]) -> usize {
        match self {
            InterfaceBlocks::Name => d
                .iter()
                .flat_map(|d| d.name.as_ref().map(|s| s.width()))
                .max()
                .unwrap_or(0),
            InterfaceBlocks::BaseClass => d
                .iter()
                .map(|d| d.class.to_string().len())
                .max()
                .unwrap_or(0),
            InterfaceBlocks::PortPath => d.iter().map(|d| d.path.len()).max().unwrap_or(0),
            InterfaceBlocks::SysPath => d
                .iter()
                .flat_map(|d| d.syspath.as_ref().map(|v| v.len()))
                .max()
                .unwrap_or(0),
            InterfaceBlocks::Driver => d
                .iter()
                .flat_map(|d| d.driver.as_ref().map(|v| v.len()))
                .max()
                .unwrap_or(0),
            InterfaceBlocks::UidClass => d
                .iter()
                .flat_map(|d| d.class_name().map(|s| s.len()))
                .max()
                .unwrap_or(0),
            InterfaceBlocks::UidSubClass => d
                .iter()
                .flat_map(|d| d.sub_class_name().map(|s| s.len()))
                .max()
                .unwrap_or(0),
            InterfaceBlocks::UidProtocol => d
                .iter()
                .flat_map(|d| d.protocol_name().map(|s| s.len()))
                .max()
                .unwrap_or(0),
            InterfaceBlocks::Class => d
                .iter()
                .map(|d| d.fully_defined_class().to_string().len())
                .max()
                .unwrap_or(0),
            _ => self.block_length().len(),
        }
    }

    fn generate_padding(d: &[&Interface]) -> HashMap<Self, usize> {
        InterfaceBlocks::iter()
            .map(|b| (b, cmp::max(b.heading().len(), b.len(d))))
            .collect()
    }

    fn colour(&self, s: &str, ct: &colour::ColourTheme) -> ColoredString {
        match self {
            InterfaceBlocks::Number => ct.number.map_or(s.normal(), |c| s.color(c)),
            InterfaceBlocks::Name => ct.name.map_or(s.normal(), |c| s.color(c)),
            InterfaceBlocks::PortPath | InterfaceBlocks::SysPath => {
                ct.path.map_or(s.normal(), |c| s.color(c))
            }
            InterfaceBlocks::Icon => ct.icon.map_or(s.normal(), |c| s.color(c)),
            InterfaceBlocks::BaseClass
            | InterfaceBlocks::UidClass
            | InterfaceBlocks::Class
            | InterfaceBlocks::BaseValue => ct.class_code.map_or(s.normal(), |c| s.color(c)),
            InterfaceBlocks::SubClass | InterfaceBlocks::UidSubClass => {
                ct.sub_code.map_or(s.normal(), |c| s.color(c))
            }
            InterfaceBlocks::Protocol | InterfaceBlocks::UidProtocol => {
                ct.protocol.map_or(s.normal(), |c| s.color(c))
            }
            InterfaceBlocks::Driver => ct.driver.map_or(s.normal(), |c| s.color(c)),
            InterfaceBlocks::AltSetting | InterfaceBlocks::NumEndpoints => {
                ct.number.map_or(s.normal(), |c| s.color(c))
            }
        }
    }

    fn format_value(
        &self,
        interface: &Interface,
        pad: &HashMap<Self, usize>,
        settings: &PrintSettings,
    ) -> Option<String> {
        match self {
            InterfaceBlocks::Number => Some(format!("{:2}", interface.number)),
            InterfaceBlocks::Name => Some(match interface.name.as_ref() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            InterfaceBlocks::NumEndpoints => Some(format!("{:2}", interface.endpoints.len())),
            InterfaceBlocks::PortPath => Some(format!(
                "{:pad$}",
                interface.path,
                pad = pad.get(self).unwrap_or(&0)
            )),
            InterfaceBlocks::SysPath => Some(match interface.syspath.as_ref() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            InterfaceBlocks::Driver => Some(match interface.driver.as_ref() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            InterfaceBlocks::BaseClass => Some(format!(
                "{:pad$}",
                interface.class.to_string(),
                pad = pad.get(self).unwrap_or(&0)
            )),
            InterfaceBlocks::SubClass => Some(Self::format_base_u8(interface.sub_class, settings)),
            InterfaceBlocks::Protocol => Some(Self::format_base_u8(interface.protocol, settings)),
            InterfaceBlocks::AltSetting => {
                Some(Self::format_base_u8(interface.alt_setting, settings))
            }
            InterfaceBlocks::Icon => settings.icons.as_ref().map(|i| {
                i.get_classifier_icon(&interface.class, interface.sub_class, interface.protocol)
            }),
            InterfaceBlocks::UidClass => Some(match interface.class_name() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            InterfaceBlocks::UidSubClass => Some(match interface.sub_class_name() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            InterfaceBlocks::UidProtocol => Some(match interface.protocol_name() {
                Some(v) => format!("{:pad$}", v, pad = pad.get(self).unwrap_or(&0)),
                None => format!("{:pad$}", "-", pad = pad.get(self).unwrap_or(&0)),
            }),
            InterfaceBlocks::Class => Some(format!(
                "{:pad$}",
                interface.fully_defined_class(),
                pad = pad.get(self).unwrap_or(&0)
            )),
            InterfaceBlocks::BaseValue => {
                Some(Self::format_base_u8(interface.class.into(), settings))
            }
        }
    }

    fn heading(&self) -> &str {
        match self {
            InterfaceBlocks::Number => "#",
            InterfaceBlocks::Name => "Name",
            InterfaceBlocks::NumEndpoints => "E#",
            InterfaceBlocks::PortPath => "PPath",
            InterfaceBlocks::SysPath => "SPath",
            InterfaceBlocks::Driver => "Driver",
            InterfaceBlocks::BaseClass => "BaseC",
            InterfaceBlocks::SubClass => "SubC",
            InterfaceBlocks::Protocol => "Pcol",
            InterfaceBlocks::AltSetting => "Alt#",
            InterfaceBlocks::UidClass => "UidCl",
            InterfaceBlocks::UidSubClass => "UidSc",
            InterfaceBlocks::UidProtocol => "UidPc",
            InterfaceBlocks::Class => "Class",
            InterfaceBlocks::BaseValue => "CVal",
            InterfaceBlocks::Icon => ICON_HEADING,
        }
    }

    fn heading_padded(&self, pad: &HashMap<Self, usize>) -> String {
        format!(
            "{:^pad$}",
            self.heading(),
            pad = pad.get(self).unwrap_or(&0)
        )
    }

    fn block_length(&self) -> BlockLength {
        match self {
            InterfaceBlocks::Icon => BlockLength::Fixed(1),
            InterfaceBlocks::Number => BlockLength::Fixed(2),
            InterfaceBlocks::NumEndpoints => BlockLength::Fixed(2),
            InterfaceBlocks::SubClass
            | InterfaceBlocks::Protocol
            | InterfaceBlocks::AltSetting
            | InterfaceBlocks::BaseValue => BlockLength::Fixed(4),
            _ => BlockLength::Variable(self.heading().len()),
        }
    }

    fn is_icon(&self) -> bool {
        self == &InterfaceBlocks::Icon
    }
}

impl Block<EndpointBlocks, Endpoint> for EndpointBlocks {
    const INSET: u8 = 3;

    fn default_blocks(verbose: bool) -> Vec<Self> {
        if verbose {
            vec![
                EndpointBlocks::Number,
                EndpointBlocks::Direction,
                EndpointBlocks::TransferType,
                EndpointBlocks::SyncType,
                EndpointBlocks::UsageType,
                EndpointBlocks::Interval,
                EndpointBlocks::MaxPacketSize,
            ]
        } else {
            vec![
                EndpointBlocks::Number,
                EndpointBlocks::Direction,
                EndpointBlocks::TransferType,
                EndpointBlocks::SyncType,
                EndpointBlocks::UsageType,
                EndpointBlocks::MaxPacketSize,
            ]
        }
    }

    fn len(&self, d: &[&Endpoint]) -> usize {
        match self {
            EndpointBlocks::TransferType => d
                .iter()
                .map(|d| d.transfer_type.to_string().len())
                .max()
                .unwrap_or(0),
            EndpointBlocks::SyncType => d
                .iter()
                .map(|d| d.sync_type.to_string().len())
                .max()
                .unwrap_or(0),
            EndpointBlocks::UsageType => d
                .iter()
                .map(|d| d.usage_type.to_string().len())
                .max()
                .unwrap_or(0),
            EndpointBlocks::Direction => d
                .iter()
                .map(|d| d.address.direction.to_string().len())
                .max()
                .unwrap_or(0),
            EndpointBlocks::MaxPacketSize => d
                .iter()
                .map(|d| d.max_packet_string().len())
                .max()
                .unwrap_or(0),
            _ => self.block_length().len(),
        }
    }

    fn generate_padding(d: &[&Endpoint]) -> HashMap<Self, usize> {
        EndpointBlocks::iter()
            .map(|b| (b, cmp::max(b.heading().len(), b.len(d))))
            .collect()
    }

    fn colour(&self, s: &str, ct: &colour::ColourTheme) -> ColoredString {
        match self {
            EndpointBlocks::Number | EndpointBlocks::Interval | EndpointBlocks::MaxPacketSize => {
                ct.number.map_or(s.normal(), |c| s.color(c))
            }
            EndpointBlocks::Direction
            | EndpointBlocks::UsageType
            | EndpointBlocks::TransferType
            | EndpointBlocks::SyncType => ct.attributes.map_or(s.normal(), |c| s.color(c)),
        }
    }

    fn format_value(
        &self,
        end: &Endpoint,
        pad: &HashMap<Self, usize>,
        _settings: &PrintSettings,
    ) -> Option<String> {
        match self {
            EndpointBlocks::Number => Some(format!("{:2}", end.address.number)),
            EndpointBlocks::Interval => Some(format!("{:2}", end.interval)),
            EndpointBlocks::MaxPacketSize => Some(format!(
                "{:pad$}",
                end.max_packet_string(),
                pad = pad.get(self).unwrap_or(&0)
            )),
            EndpointBlocks::Direction => Some(format!(
                "{:pad$}",
                end.address.direction.to_string(),
                pad = pad.get(self).unwrap_or(&0)
            )),
            EndpointBlocks::TransferType => Some(format!(
                "{:pad$}",
                end.transfer_type.to_string(),
                pad = pad.get(self).unwrap_or(&0)
            )),
            EndpointBlocks::SyncType => Some(format!(
                "{:pad$}",
                end.sync_type.to_string(),
                pad = pad.get(self).unwrap_or(&0)
            )),
            EndpointBlocks::UsageType => Some(format!(
                "{:pad$}",
                end.usage_type.to_string(),
                pad = pad.get(self).unwrap_or(&0)
            )),
        }
    }

    fn heading(&self) -> &str {
        match self {
            EndpointBlocks::Number => "#",
            EndpointBlocks::Interval => "Iv",
            EndpointBlocks::MaxPacketSize => "MaxPkb",
            EndpointBlocks::Direction => "Dir",
            EndpointBlocks::TransferType => "TranT",
            EndpointBlocks::SyncType => "SyncT",
            EndpointBlocks::UsageType => "UsgeT",
        }
    }

    fn heading_padded(&self, pad: &HashMap<Self, usize>) -> String {
        format!(
            "{:^pad$}",
            self.heading(),
            pad = pad.get(self).unwrap_or(&0)
        )
    }

    fn block_length(&self) -> BlockLength {
        match self {
            EndpointBlocks::Number => BlockLength::Fixed(2),
            EndpointBlocks::Interval => BlockLength::Fixed(2),
            _ => BlockLength::Variable(self.heading().len()),
        }
    }
}

/// Value to sort [`Device`]
#[derive(Default, PartialEq, Eq, Debug, ValueEnum, Clone, Copy, Serialize, Deserialize)]
pub enum Sort {
    #[default]
    /// Sort by bus device number
    DeviceNumber,
    /// Sort by position in parent branch
    BranchPosition,
    /// No sorting; whatever order it was parsed
    NoSort,
}

impl Sort {
    /// Sort the [`Device`]s in place
    pub fn sort_devices(&self, devices: &mut [Device]) {
        // add bus number to maintain bus order when sorting
        match self {
            Sort::BranchPosition => {
                devices.sort_by_key(|d| d.get_branch_position() + d.location_id.bus)
            }
            Sort::DeviceNumber => devices.sort_by_key(|d| d.location_id.number + d.location_id.bus),
            _ => (),
        }
    }

    /// Sort the references to [`Device`]s in place
    pub fn sort_devices_ref(&self, devices: &mut [&Device]) {
        match self {
            Sort::BranchPosition => {
                devices.sort_by_key(|d| d.get_branch_position() + d.location_id.bus)
            }
            Sort::DeviceNumber => devices.sort_by_key(|d| d.location_id.number + d.location_id.bus),
            _ => (),
        }
    }

    /// Sort the devices at each branch by calling this recursively after sorting the devices at this level
    pub fn sort_devices_recursive(&self, devices: &mut Vec<Device>) {
        // sort the devices at this level
        self.sort_devices(devices);
        // then sort the devices at each branch
        for device in devices {
            if let Some(branch_devices) = &mut device.devices {
                self.sort_devices_recursive(branch_devices);
            }
        }
    }

    /// Walk the bus tree and sort the devices at each branch
    pub fn sort_bus(&self, bus: &mut Bus) {
        if matches!(self, Sort::NoSort) {
            return;
        }

        if let Some(devices) = &mut bus.devices {
            self.sort_devices_recursive(devices);
        }
    }

    /// Sort buses in place, sorting devices on each bus and then by bus number
    pub fn sort_buses(&self, buses: &mut Vec<Bus>) {
        buses.sort_by_key(|b| b.get_bus_number());
        for bus in buses {
            self.sort_bus(bus);
        }
    }
}

/// Value to group [`Device`]
#[derive(Default, Debug, ValueEnum, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Group {
    #[default]
    /// No grouping
    NoGroup,
    /// Group into buses with bus info as heading - like a flat tree
    Bus,
}

/// Options for [`PrintSettings`] mask_serials
#[derive(Default, Debug, ValueEnum, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaskSerial {
    /// Hide with '*' char
    #[default]
    Hide,
    /// Mask by randomising existing chars
    Scramble,
    /// Mask by replacing length with random chars
    Replace,
}

/// Mode being used for printing
#[derive(Default, Debug, ValueEnum, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrintMode {
    /// Normal printing to static output
    #[default]
    Normal,
    /// Dynamic printing such as watch mode
    Dynamic,
}

/// Passed to printing functions allows default args
#[derive(Debug, Default)]
pub struct PrintSettings {
    /// Don't pad in order to align blocks
    pub no_padding: bool,
    /// Print in decimal not base16
    pub decimal: bool,
    /// No tree printing
    pub tree: bool,
    /// Sort devices
    pub sort_devices: Sort,
    /// Sort buses by bus number
    pub sort_buses: bool,
    /// Group devices
    pub group_devices: Group,
    /// Print headings for blocks
    pub headings: bool,
    /// Level of verbosity
    pub verbosity: u8,
    /// Print more blocks by default
    pub more: bool,
    /// Print as json
    pub json: bool,
    /// Character encoding to use
    pub encoding: Encoding,
    /// Scramble serial numbers, useful if sharing sensitive device dumps
    pub mask_serials: Option<MaskSerial>,
    /// [`DeviceBlocks`] to use for printing
    pub device_blocks: Option<Vec<DeviceBlocks>>,
    /// [`BusBlocks`] to use for printing
    pub bus_blocks: Option<Vec<BusBlocks>>,
    /// [`ConfigurationBlocks`] to use for printing
    pub config_blocks: Option<Vec<ConfigurationBlocks>>,
    /// [`InterfaceBlocks`] to use for printing
    pub interface_blocks: Option<Vec<InterfaceBlocks>>,
    /// [`EndpointBlocks`] to use for printing
    pub endpoint_blocks: Option<Vec<EndpointBlocks>>,
    /// [`crate::icon::IconTheme`] to apply - None to not print any icons
    pub icons: Option<icon::IconTheme>,
    /// [`crate::colour::ColourTheme`] to apply - None to not colour
    pub colours: Option<colour::ColourTheme>,
    /// Max variable string length to display before truncating - descriptors and classes for example
    pub max_variable_string_len: Option<usize>,
    /// Enable auto generation of max_variable_string_len based on terminal width
    pub auto_width: bool,
    /// Terminal width and height data
    pub terminal_size: Option<(u16, u16)>,
    /// When to print icon blocks
    pub icon_when: IconWhen,
    /// When to print colour
    pub color_when: ColorWhen,
    /// Printing in watch mode
    pub print_mode: PrintMode,
}

/// Converts a HashSet of [`ConfigAttributes`] a String of nerd icons
fn attributes_to_icons(attributes: &Vec<ConfigAttributes>, settings: &PrintSettings) -> String {
    let mut icon_strs = Vec::new();
    if settings.icons.is_some() {
        for a in attributes {
            match a {
                ConfigAttributes::SelfPowered => icon_strs.push("\u{f06a5}"), // 󰚥
                ConfigAttributes::RemoteWakeup => icon_strs.push("\u{f0155}"), // 󰅕
                ConfigAttributes::BatteryPowered => icon_strs.push("\u{f244}"), // 
                ConfigAttributes::BusPowered => icon_strs.push("\u{f11f0}"),  // 󱇰
            }
        }
    }
    icon_strs.join(" ")
}

/// Truncates and appends '...' to show string has been truncated
///
/// `len` is length of resulting String, with '...' so original `s` content will be len - 3
///
/// If `len` is less than 3, `s` truncated to this length
///
/// ```
/// use cyme::display::truncate_string;
/// let mut string = String::from("Hello world");
/// truncate_string(&mut string, 8);
/// assert_eq!(string, "Hello...");
/// // emoji are 2 bytes so will be truncated correctly on char boundary
/// let mut string = String::from("Hell😅 world");
/// truncate_string(&mut string, 8);
/// assert_eq!(string, "Hell😅...");
/// let mut string = String::from("bl");
/// truncate_string(&mut string, 2);
/// assert_eq!(string, "bl");
/// // don't shorten if already length
/// let mut string = String::from("blah");
/// truncate_string(&mut string, 4);
/// assert_eq!(string, "blah");
/// // just over length
/// let mut string = String::from("blahx");
/// truncate_string(&mut string, 4);
/// assert_eq!(string, "b...");
/// ```
pub fn truncate_string(s: &mut String, len: usize) {
    // if already less than or equal to len, or len is less than 3, return
    if s.width() <= len || len <= 3 {
        return;
    }
    // use char_indices to find last char boundary before len - 3
    // not s.len() as this is the byte length and utf-8 chars can be multiple bytes
    if let Some((i, _)) = s.char_indices().nth(len - 3) {
        s.truncate(i);
        s.push_str("...");
    }
}

/// Finds the maximum string size to truncate variable fields
///
/// Calculates based on the [`PrintSettings`] terminal_size width, the total length of the [`BlockLength::Fixed`] fields and thus the remaining space to divide between [`BlockLength::Variable`] fields as the maximum string size
///
/// Total length is based the prior calculated `variable_lens` - the values represent the maximum length of variable fields to print
pub fn auto_max_string_len<B: BlockEnum, T>(
    blocks: &[impl Block<B, T>],
    offset: usize,
    #[allow(clippy::ptr_arg)] variable_lens: &Vec<usize>,
    settings: &PrintSettings,
) -> Option<usize> {
    if variable_lens.is_empty() {
        return None;
    }

    // total fixed includes length of blocks to account for spaces between fields, plus tree offset
    let total_fixed: usize = blocks
        .iter()
        .filter_map(|b| b.block_length().fixed_len())
        .sum::<usize>()
        + blocks.len()
        + offset;
    let total_variable: usize = variable_lens.iter().sum();
    let total_len: usize = total_fixed + total_variable + (blocks.len() * 2);
    let (width, height) = settings.terminal_size.unwrap_or((DEFAULT_AUTO_WIDTH, 0));
    log::trace!(
        "Auto scaling running for max length {total_len:?} of which fixed {total_fixed:?}, to terminal size {width:?} {height:?}"
    );
    let w = width as usize;

    if total_len > w {
        // fixed already taking all space, return min
        if w < total_fixed {
            log::trace!("Cannot scale, fixed already taking all space!");
            return Some(MIN_VARIABLE_STRING_LEN);
        }
        // remaining len for variable strings
        let variable_len_remain: usize = w - total_fixed;
        // auto max is the space not taken by fixed divided by number of variable length
        // *variable_lens checked not zero at entry so should not be div 0
        let mut auto_max_string = variable_len_remain / (variable_lens.len());
        // remaining chars are those not used by variable strings; ones not over the found auto max and can be used by other variable strings - bumping the global max up since they won't use it
        let mut remaining_chars: usize = variable_lens
            .iter()
            .filter(|v| **v <= auto_max_string)
            .map(|v| auto_max_string - v)
            .sum();
        log::trace!(
            "Auto max string calculated {auto_max_string:?}, remaining {remaining_chars:?}"
        );

        // equally divide remaining chars between variable > auto_max_string - not perfect as could be shared per how much longer each is but this would require unique max for each block
        let variable_longer = variable_lens
            .iter()
            .filter(|v| **v > auto_max_string)
            .count();
        if variable_longer != 0 {
            remaining_chars /= variable_longer;
        }
        auto_max_string += remaining_chars;

        if auto_max_string < MIN_VARIABLE_STRING_LEN {
            log::trace!(
                "Ignoring auto max string {auto_max_string:?}! Clamped to MIN_VARIABLE_STRING_LEN {MIN_VARIABLE_STRING_LEN:?}"
            );
            Some(MIN_VARIABLE_STRING_LEN)
        } else {
            log::trace!("Final auto max string {auto_max_string:?}");
            Some(auto_max_string)
        }
    } else {
        None
    }
}

/// Returns true if the [`Block`] has a valid icon for the [`PrintSettings`] [`Encoding`]
pub fn has_valid_icons<B: BlockEnum, T>(
    d: &T,
    blocks: &[impl Block<B, T>],
    settings: &PrintSettings,
) -> bool {
    blocks.iter().filter(|b| b.is_icon()).all(|b| {
        if log::log_enabled!(log::Level::Trace) {
            let val = b.format_value(d, &HashMap::new(), settings);
            let ret = match &val {
                Some(v) => settings.encoding.str_is_valid(v),
                None => false,
            };
            log::trace!(
                "icon {:?} valid for {:?}: {:?}",
                val,
                settings.encoding,
                ret
            );
            ret
        } else {
            match b.format_value(d, &HashMap::new(), settings) {
                Some(v) => settings.encoding.str_is_valid(&v),
                None => false,
            }
        }
    })
}

/// Formats each [`Block`] value shown from a device `d`
pub fn render_value<B: BlockEnum, T>(
    d: &T,
    blocks: &[impl Block<B, T>],
    pad: &HashMap<B, usize>,
    settings: &PrintSettings,
    max_string_length: Option<usize>,
    dimmed: bool,
) -> Vec<String> {
    let mut ret = Vec::new();
    for b in blocks {
        if let Some(mut string) = b.format_value(d, pad, settings) {
            // truncate if max_string_length present and before colour applied as this will _add_ chars
            if b.value_is_variable_length() {
                if let Some(ml) = max_string_length {
                    truncate_string(&mut string, ml)
                }
            }
            match &settings.colours {
                Some(c) => {
                    if dimmed {
                        ret.push(format!("{}", string.dimmed().white()))
                    } else {
                        ret.push(format!("{}", b.colour(&string, c)))
                    }
                }
                None => ret.push(string.to_string()),
            };
        }
    }

    ret
}

/// Renders the headings for each [`Block`] being shown
pub fn render_heading<B: BlockEnum, T>(
    blocks: &[impl Block<B, T>],
    pad: &HashMap<B, usize>,
    max_string_length: Option<usize>,
) -> Vec<String> {
    let mut ret = Vec::new();

    for b in blocks {
        let mut string = b.heading_padded(pad);
        if b.value_is_variable_length() {
            if let Some(ml) = max_string_length {
                truncate_string(&mut string, ml)
            }
        }
        ret.push(string)
    }

    ret
}

/// Generates tree formatting and values given `current_tree`, current `branch_length` and item `index` in branch
fn generate_tree_data(
    current_tree: &TreeData,
    branch_length: usize,
    index: usize,
    settings: &PrintSettings,
) -> TreeData {
    let mut pass_tree = current_tree.clone();

    // get prefix from icons if tree - maybe should cache these before build rather than lookup each time...
    if settings.tree {
        pass_tree.prefix = if pass_tree.depth > 0 {
            let edge_icon = if index + 1 != pass_tree.branch_length {
                icon::Icon::TreeLine
            } else {
                icon::Icon::TreeBlank
            };

            format!(
                "{}{}",
                pass_tree.prefix,
                settings.icons.as_ref().map_or(
                    icon::get_default_tree_icon(&edge_icon, &settings.encoding),
                    |i| i.get_tree_icon(&edge_icon, &settings.encoding)
                )
            )
        } else {
            pass_tree.prefix.to_string()
        };
    }

    pass_tree.depth += 1;
    pass_tree.branch_length = branch_length;
    pass_tree.trunk_index = index as u8;

    pass_tree
}

/// Generates the [`DeviceExtra`] blocks based on the [`PrintSettings`] or defaults. Will also retain based on `is_icon` and [`IconWhen`] setting
///
/// If [`IconWhen::Auto`] will render icon block values to check if supported by [`Encoding`] and remove if not
fn generate_extra_blocks(
    extra: &DeviceExtra,
    settings: &PrintSettings,
) -> (
    Vec<ConfigurationBlocks>,
    Vec<InterfaceBlocks>,
    Vec<EndpointBlocks>,
) {
    let mut blocks = (
        settings.config_blocks.to_owned().unwrap_or(
            Block::<ConfigurationBlocks, Configuration>::default_blocks(settings.more),
        ),
        settings.interface_blocks.to_owned().unwrap_or(
            Block::<InterfaceBlocks, Interface>::default_blocks(settings.more),
        ),
        settings.endpoint_blocks.to_owned().unwrap_or(
            Block::<EndpointBlocks, Endpoint>::default_blocks(settings.more),
        ),
    );

    // auto drop icon blocks depending on IconWhen and Encoding
    // will drop if any in search is not valid for encoding rather than per device
    // I think accepable as similar to device block behaviour
    match settings.icon_when {
        // if never or auto and no icons, drop
        IconWhen::Never | IconWhen::Auto if settings.icons.is_none() => {
            blocks.0.retain(|b| !b.is_icon());
            blocks.1.retain(|b| !b.is_icon());
            blocks.2.retain(|b| !b.is_icon());
        }
        // skip further processing if including private use area utf8
        IconWhen::Auto if settings.encoding == Encoding::Glyphs => (),
        // always only warn if no icons provided
        IconWhen::Always => {
            if settings.icons.is_none() {
                log::warn!(
                    "{:?} blocks requested but no icons provided",
                    settings.icon_when
                );
            }
        }
        // drill through values checking
        _ => {
            settings
                .icon_when
                .retain(&extra.configurations, &mut blocks.0, settings);
            extra.configurations.iter().for_each(|c| {
                settings
                    .icon_when
                    .retain(&c.interfaces, &mut blocks.1, settings);
                c.interfaces.iter().for_each(|i| {
                    settings
                        .icon_when
                        .retain(&i.endpoints, &mut blocks.2, settings);
                });
            });
        }
    }
    blocks
}

/// Passed to print functions to support tree building
#[derive(Debug, Default, Clone)]
pub struct TreeData {
    /// Length of the branch sitting on
    branch_length: usize,
    /// Index within parent list of devices
    trunk_index: u8,
    /// Depth of tree being built - normally len() tree_positions but might not be if printing inner
    depth: usize,
    /// Prefix to apply, builds up as depth increases
    prefix: String,
}

/// The operation to perform on the blocks when specified by the user
#[derive(Default, PartialEq, Eq, Debug, ValueEnum, Clone, Copy, Serialize, Deserialize)]
pub enum BlockOperation {
    /// Add new blocks to the existing blocks, ignoring duplicates
    Add,
    /// Append new blocks to the end of the existing blocks
    Append,
    /// Replace all blocks with new ones
    #[default]
    New,
    /// Prepend new blocks to the start of the existing blocks
    Prepend,
    /// Remove matching blocks from the existing blocks
    Remove,
}

impl BlockOperation {
    /// Create a new or run the operation on the blocks, returning the new blocks
    pub fn new_or_op<B: BlockEnum + Block<B, T>, T>(
        &self,
        blocks: Option<Vec<B>>,
        new: &[B],
        verbose: bool,
    ) -> Result<Vec<B>> {
        if matches!(self, BlockOperation::New) {
            return Ok(new.to_vec());
        }

        let mut current = blocks.unwrap_or_else(|| B::default_blocks(verbose));
        self.run(&mut current, new)?;
        Ok(current)
    }

    /// Run the operation on the blocks, modifying them in place
    pub fn run<T: BlockEnum>(&self, blocks: &mut Vec<T>, new: &[T]) -> Result<()> {
        match self {
            BlockOperation::New => {
                *blocks = new.to_vec();
            }
            BlockOperation::Append => {
                blocks.extend(new.iter().cloned());
            }
            BlockOperation::Prepend => {
                let mut new = new.to_vec();
                new.append(blocks);
                *blocks = new;
            }
            BlockOperation::Add => {
                for b in new {
                    if !blocks.contains(b) {
                        blocks.push(b.clone());
                    }
                }
            }
            BlockOperation::Remove => {
                for b in new {
                    blocks.retain(|x| x != b);
                }
            }
        }
        Ok(())
    }
}

/// Used to describe the item being printed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineItem {
    /// Bus number
    Bus(usize),
    /// Device port path
    Device(PortPath),
    /// Configuration number
    Config(ConfigurationPath),
    /// Interface name
    Interface(DevicePath),
    /// Endpoint Address
    Endpoint(EndpointPath),
    /// New lines or other non-item
    None,
}

/// DisplayWriter allows control of output to terminal or other Writer
///
/// Mainly for watch mode to allow control of output
pub struct DisplayWriter<W: Write> {
    raw_mode: bool,
    line_context: Vec<LineItem>,
    inner: W,
}

impl<W: Write> Write for DisplayWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Default for DisplayWriter<io::Stdout> {
    fn default() -> Self {
        Self::new(io::stdout())
    }
}

impl<W: Write> DisplayWriter<W> {
    /// Create a new DisplayWriter with the inner writer
    pub fn new(inner: W) -> Self {
        Self {
            raw_mode: false,
            line_context: Vec::new(),
            inner,
        }
    }

    /// Set the raw mode for the writer
    ///
    /// Raw mode will print `\r\n` instead of `\n` for newlines
    pub fn set_raw_mode(&mut self, raw_mode: bool) {
        self.raw_mode = raw_mode;
    }

    /// Print text to the writer
    pub fn print<S: AsRef<str>>(&mut self, text: S) -> io::Result<()> {
        write!(self.inner, "{}", text.as_ref())?;
        self.inner.flush()?;
        Ok(())
    }

    /// Print text to the writer with a newline
    pub fn println<S: AsRef<str>>(&mut self, text: S, item: LineItem) -> io::Result<()> {
        if self.raw_mode {
            write!(self.inner, "{}\r\n", text.as_ref())?;
        } else {
            writeln!(self.inner, "{}", text.as_ref())?;
        }
        self.line_context.push(item);
        self.inner.flush()?;
        Ok(())
    }

    /// Get the inner writer
    pub fn into_inner(self) -> W {
        self.inner
    }

    /// Get the line context for the writer
    pub fn line_context(&self) -> &Vec<LineItem> {
        &self.line_context
    }

    /// All device [`Endpoint`]
    pub fn print_endpoints(
        &mut self,
        interface: &Interface,
        blocks: &[EndpointBlocks],
        settings: &PrintSettings,
        tree: &TreeData,
        dimmed: bool,
    ) {
        let endpoints = &interface.endpoints;
        let device_path = interface.device_path();
        let mut pad = if !settings.no_padding {
            let endpoints: Vec<&Endpoint> = endpoints.iter().collect();
            EndpointBlocks::generate_padding(&endpoints)
        } else {
            HashMap::new()
        };
        pad.retain(|k, _| blocks.contains(k));

        let max_variable_string_len: Option<usize> = if settings.auto_width {
            let offset = if settings.tree {
                tree.depth * 3 + 1
            } else {
                (EndpointBlocks::INSET * LIST_INSET_SPACES) as usize
            };
            let variable_lens: Vec<usize> = pad
                .iter()
                .filter(|(k, _)| k.value_is_variable_length())
                .map(|(_, v)| *v)
                .collect();
            auto_max_string_len(blocks, offset, &variable_lens, settings)
                .or(settings.max_variable_string_len)
        } else {
            settings.max_variable_string_len
        };

        log::trace!("Print endpoints padding {pad:?}, tree {tree:?}");

        // if there is a max variable length, adjust padding to this if current > it and is variable
        if let Some(ml) = max_variable_string_len.as_ref() {
            for (k, v) in pad.iter_mut() {
                if k.value_is_variable_length() {
                    *v = cmp::min(*v, *ml);
                }
            }
        }

        for (i, endpoint) in endpoints.iter().enumerate() {
            let line_item = if let Some(dp) = device_path.as_ref() {
                LineItem::Endpoint(EndpointPath::new_with_device_path(
                    dp.to_owned(),
                    endpoint.address.address,
                ))
            } else {
                LineItem::None
            };
            // get current prefix based on if last in tree and whether we are within the tree
            if settings.tree {
                let mut prefix = if tree.depth > 0 {
                    let edge_icon = if i + 1 != tree.branch_length {
                        icon::Icon::TreeEdge
                    } else {
                        icon::Icon::TreeCorner
                    };
                    let edge = settings.icons.as_ref().map_or(
                        icon::get_default_tree_icon(&edge_icon, &settings.encoding),
                        |i| i.get_tree_icon(&edge_icon, &settings.encoding),
                    );
                    format!("{}{}", tree.prefix, edge)
                // zero depth
                } else {
                    tree.prefix.to_string()
                };

                let mut terminator = settings.icons.as_ref().map_or(
                    icon::get_default_tree_icon(
                        &icon::Icon::Endpoint(endpoint.address.direction),
                        &settings.encoding,
                    ),
                    |i| {
                        i.get_tree_icon(
                            &icon::Icon::Endpoint(endpoint.address.direction),
                            &settings.encoding,
                        )
                    },
                );

                // colour tree
                if let Some(ct) = settings.colours.as_ref() {
                    prefix = ct
                        .tree
                        .map_or(prefix.normal(), |c| prefix.color(c))
                        .to_string();
                    terminator = if endpoint.address.direction == Direction::In {
                        ct.tree_endpoint_in
                            .map_or(terminator.normal(), |c| terminator.color(c))
                            .to_string()
                    } else {
                        ct.tree_endpoint_out
                            .map_or(terminator.normal(), |c| terminator.color(c))
                            .to_string()
                    };
                }

                // maybe should just do once at start of bus
                if settings.headings && i == 0 {
                    let heading = render_heading(blocks, &pad, max_variable_string_len).join(" ");
                    self.println(
                        format!("{}  {}", prefix, heading.bold().underline()),
                        LineItem::None,
                    )
                    .unwrap();
                }

                // render and print tree if doing it
                self.print(format!("{prefix}{terminator} ")).unwrap();
                self.println(
                    render_value(
                        endpoint,
                        blocks,
                        &pad,
                        settings,
                        max_variable_string_len,
                        dimmed,
                    )
                    .join(" "),
                    line_item,
                )
                .unwrap();
            } else {
                if settings.headings && i == 0 {
                    let heading = render_heading(blocks, &pad, max_variable_string_len).join(" ");
                    self.println(
                        format!("{:spaces$}{}", "", heading.bold().underline(), spaces = 6),
                        LineItem::None,
                    )
                    .unwrap();
                }

                self.println(
                    format!(
                        "{:spaces$}{}",
                        "",
                        render_value(
                            endpoint,
                            blocks,
                            &pad,
                            settings,
                            max_variable_string_len,
                            dimmed
                        )
                        .join(" "),
                        spaces = (EndpointBlocks::INSET * LIST_INSET_SPACES) as usize
                    ),
                    line_item,
                )
                .unwrap();
            }
        }
    }

    /// All device [`Interface`]
    pub fn print_interfaces(
        &mut self,
        interfaces: &[Interface],
        blocks: (&Vec<InterfaceBlocks>, &Vec<EndpointBlocks>),
        settings: &PrintSettings,
        tree: &TreeData,
        dimmed: bool,
    ) {
        let mut pad = if !settings.no_padding {
            let interfaces: Vec<&Interface> = interfaces.iter().collect();
            InterfaceBlocks::generate_padding(&interfaces)
        } else {
            HashMap::new()
        };
        pad.retain(|k, _| blocks.0.contains(k));

        let max_variable_string_len: Option<usize> = if settings.auto_width {
            let offset = if settings.tree {
                tree.depth * 3 + 1
            } else {
                (InterfaceBlocks::INSET * LIST_INSET_SPACES) as usize
            };
            let variable_lens: Vec<usize> = pad
                .iter()
                .filter(|(k, _)| k.value_is_variable_length())
                .map(|(_, v)| *v)
                .collect();
            auto_max_string_len(blocks.0, offset, &variable_lens, settings)
                .or(settings.max_variable_string_len)
        } else {
            settings.max_variable_string_len
        };

        // if there is a max variable length, adjust padding to this if current > it
        if let Some(ml) = max_variable_string_len.as_ref() {
            for (k, v) in pad.iter_mut() {
                if k.value_is_variable_length() {
                    *v = cmp::min(*v, *ml);
                }
            }
        }

        log::trace!("Print interfaces padding {pad:?}, tree {tree:?}");

        for (i, interface) in interfaces.iter().enumerate() {
            let line_item = if let Some(dp) = interface.device_path() {
                LineItem::Interface(dp)
            } else {
                LineItem::None
            };
            // get current prefix based on if last in tree and whether we are within the tree
            if settings.tree {
                let mut prefix = if tree.depth > 0 {
                    let edge_icon = if i + 1 != tree.branch_length {
                        icon::Icon::TreeEdge
                    } else {
                        icon::Icon::TreeCorner
                    };
                    let edge = settings.icons.as_ref().map_or(
                        icon::get_default_tree_icon(&edge_icon, &settings.encoding),
                        |i| i.get_tree_icon(&edge_icon, &settings.encoding),
                    );
                    format!("{}{}", tree.prefix, edge)
                // zero depth
                } else {
                    tree.prefix.to_string()
                };

                let mut terminator = settings.icons.as_ref().map_or(
                    icon::get_default_tree_icon(
                        &icon::Icon::TreeInterfaceTerminator,
                        &settings.encoding,
                    ),
                    |i| i.get_tree_icon(&icon::Icon::TreeInterfaceTerminator, &settings.encoding),
                );

                // colour tree
                if let Some(ct) = settings.colours.as_ref() {
                    prefix = ct
                        .tree
                        .map_or(prefix.normal(), |c| prefix.color(c))
                        .to_string();
                    terminator = ct
                        .tree_interface_terminator
                        .map_or(terminator.normal(), |c| terminator.color(c))
                        .to_string();
                }

                // maybe should just do once at start of bus
                if settings.headings && i == 0 {
                    let heading = render_heading(blocks.0, &pad, max_variable_string_len).join(" ");
                    self.println(
                        format!("{}  {}", prefix, heading.bold().underline()),
                        LineItem::None,
                    )
                    .unwrap();
                }

                // render and print tree if doing it
                self.print(format!("{prefix}{terminator} ")).unwrap();

                self.println(
                    render_value(
                        interface,
                        blocks.0,
                        &pad,
                        settings,
                        max_variable_string_len,
                        dimmed,
                    )
                    .join(" "),
                    line_item,
                )
                .unwrap();
            } else {
                if settings.headings && i == 0 {
                    let heading = render_heading(blocks.0, &pad, max_variable_string_len).join(" ");
                    self.println(
                        format!("{:spaces$}{}", "", heading.bold().underline(), spaces = 4),
                        LineItem::None,
                    )
                    .unwrap();
                }

                self.println(
                    format!(
                        "{:spaces$}{}",
                        "",
                        render_value(
                            interface,
                            blocks.0,
                            &pad,
                            settings,
                            max_variable_string_len,
                            dimmed
                        )
                        .join(" "),
                        spaces = (InterfaceBlocks::INSET * LIST_INSET_SPACES) as usize
                    ),
                    line_item,
                )
                .unwrap();
            }

            // print the endpoints
            if settings.verbosity >= 3 || interface.is_expanded() {
                self.print_endpoints(
                    interface,
                    blocks.1,
                    settings,
                    &generate_tree_data(tree, interface.endpoints.len(), i, settings),
                    dimmed,
                );
            }
        }
    }

    /// All device [`Configuration`]
    pub fn print_configurations(
        &mut self,
        device: &Device,
        configs: &[Configuration],
        blocks: (
            &Vec<ConfigurationBlocks>,
            &Vec<InterfaceBlocks>,
            &Vec<EndpointBlocks>,
        ),
        settings: &PrintSettings,
        tree: &TreeData,
    ) {
        let mut pad = if !settings.no_padding {
            let configs: Vec<&Configuration> = configs.iter().collect();
            ConfigurationBlocks::generate_padding(&configs)
        } else {
            HashMap::new()
        };
        pad.retain(|k, _| blocks.0.contains(k));

        let max_variable_string_len: Option<usize> = if settings.auto_width {
            let offset = if settings.tree {
                tree.depth * 3 + 1
            } else {
                (ConfigurationBlocks::INSET * LIST_INSET_SPACES) as usize
            };
            let variable_lens: Vec<usize> = pad
                .iter()
                .filter(|(k, _)| k.value_is_variable_length())
                .map(|(_, v)| *v)
                .collect();
            auto_max_string_len(blocks.0, offset, &variable_lens, settings)
                .or(settings.max_variable_string_len)
        } else {
            settings.max_variable_string_len
        };

        // if there is a max variable length, adjust padding to this if current > it
        if let Some(ml) = max_variable_string_len.as_ref() {
            for (k, v) in pad.iter_mut() {
                if k.value_is_variable_length() {
                    *v = cmp::min(*v, *ml);
                }
            }
        }

        log::trace!("Print configs padding {pad:?}, tree {tree:?}");

        for (i, config) in configs.iter().enumerate() {
            let line_item = LineItem::Config((device.port_path(), config.number));
            // get current prefix based on if last in tree and whether we are within the tree
            if settings.tree {
                let mut prefix = if tree.depth > 0 {
                    let edge_icon = if i + 1 != tree.branch_length {
                        icon::Icon::TreeEdge
                    } else {
                        icon::Icon::TreeCorner
                    };
                    let edge = settings.icons.as_ref().map_or(
                        icon::get_default_tree_icon(&edge_icon, &settings.encoding),
                        |i| i.get_tree_icon(&edge_icon, &settings.encoding),
                    );
                    format!("{}{}", tree.prefix, edge)
                // zero depth
                } else {
                    tree.prefix.to_string()
                };

                let mut terminator = settings.icons.as_ref().map_or(
                    icon::get_default_tree_icon(
                        &icon::Icon::TreeConfigurationTerminator,
                        &settings.encoding,
                    ),
                    |i| {
                        i.get_tree_icon(
                            &icon::Icon::TreeConfigurationTerminator,
                            &settings.encoding,
                        )
                    },
                );

                // colour tree
                if let Some(ct) = settings.colours.as_ref() {
                    prefix = ct
                        .tree
                        .map_or(prefix.normal(), |c| prefix.color(c))
                        .to_string();
                    terminator = ct
                        .tree_configuration_terminator
                        .map_or(terminator.normal(), |c| terminator.color(c))
                        .to_string();
                }

                // maybe should just do once at start of bus
                if settings.headings && i == 0 {
                    let heading = render_heading(blocks.0, &pad, max_variable_string_len).join(" ");
                    self.println(
                        format!("{}  {}", prefix, heading.bold().underline()),
                        LineItem::None,
                    )
                    .unwrap();
                }

                // render and print tree if doing it
                self.print(format!("{prefix}{terminator} ")).unwrap();

                self.println(
                    render_value(
                        config,
                        blocks.0,
                        &pad,
                        settings,
                        max_variable_string_len,
                        device.is_disconnected(),
                    )
                    .join(" "),
                    line_item,
                )
                .unwrap();
            } else {
                if settings.headings && i == 0 {
                    let heading = render_heading(blocks.0, &pad, max_variable_string_len).join(" ");
                    self.println(
                        format!("{:spaces$}{}", "", heading.bold().underline(), spaces = 2),
                        LineItem::None,
                    )
                    .unwrap();
                }

                self.println(
                    format!(
                        "{:spaces$}{}",
                        "",
                        render_value(
                            config,
                            blocks.0,
                            &pad,
                            settings,
                            max_variable_string_len,
                            device.is_disconnected()
                        )
                        .join(" "),
                        spaces = (ConfigurationBlocks::INSET * LIST_INSET_SPACES) as usize
                    ),
                    line_item,
                )
                .unwrap();
            }

            // print the interfaces
            if settings.verbosity >= 2 || config.is_expanded() {
                self.print_interfaces(
                    &config.interfaces,
                    ((blocks.1), (blocks.2)),
                    settings,
                    &generate_tree_data(tree, config.interfaces.len(), i, settings),
                    device.is_disconnected(),
                );
            }
        }
    }

    /// Recursively print `devices`; will call for each `Device` devices if `Some`
    ///
    /// Will draw tree if `settings.tree`, otherwise it will be flat
    pub fn print_devices(
        &mut self,
        devices: &[Device],
        db: &Vec<DeviceBlocks>,
        settings: &PrintSettings,
        tree: &TreeData,
        padding: &HashMap<DeviceBlocks, usize>,
    ) {
        let mut padding = padding.clone();
        let max_variable_string_len: Option<usize> = if settings.auto_width {
            let offset = if settings.tree { tree.depth * 3 + 1 } else { 0 };
            let variable_lens: Vec<usize> = padding
                .iter()
                .filter(|(k, _)| k.value_is_variable_length())
                .map(|(_, v)| *v)
                .collect();
            auto_max_string_len(db, offset, &variable_lens, settings)
                .or(settings.max_variable_string_len)
        } else {
            settings.max_variable_string_len
        };

        // if there is a max variable length, adjust padding to this if current > it
        if let Some(ml) = max_variable_string_len.as_ref() {
            for (k, v) in padding.iter_mut() {
                if k.value_is_variable_length() {
                    *v = cmp::min(*v, *ml);
                }
            }
        }

        log::trace!("Print devices padding {padding:?}, tree {tree:?}");

        for (i, device) in devices.iter().filter(|d| !d.is_hidden()).enumerate() {
            // get current prefix based on if last in tree and whether we are within the tree
            if settings.tree {
                let mut prefix = if tree.depth > 0 {
                    let edge_icon = if i + 1 != tree.branch_length {
                        icon::Icon::TreeEdge
                    } else {
                        icon::Icon::TreeCorner
                    };
                    let edge = settings.icons.as_ref().map_or(
                        icon::get_default_tree_icon(&edge_icon, &settings.encoding),
                        |i| i.get_tree_icon(&edge_icon, &settings.encoding),
                    );
                    format!("{}{}", tree.prefix, edge)
                // zero depth
                } else {
                    tree.prefix.to_string()
                };

                let icon_terminator = if device.is_disconnected() {
                    icon::Icon::TreeDisconnectedTerminator
                } else {
                    icon::Icon::TreeDeviceTerminator
                };
                let mut terminator = settings.icons.as_ref().map_or(
                    icon::get_default_tree_icon(&icon_terminator, &settings.encoding),
                    |i| i.get_tree_icon(&icon_terminator, &settings.encoding),
                );

                // colour tree
                if let Some(ct) = settings.colours.as_ref() {
                    prefix = ct
                        .tree
                        .map_or(prefix.normal(), |c| prefix.color(c))
                        .to_string();
                    terminator = ct
                        .tree_bus_terminator
                        .map_or(terminator.normal(), |c| terminator.color(c))
                        .to_string();
                }

                // maybe should just do once at start of bus
                if settings.headings && i == 0 {
                    let heading = render_heading(db, &padding, max_variable_string_len).join(" ");
                    self.println(
                        format!("{}  {}", prefix, heading.bold().underline()),
                        LineItem::None,
                    )
                    .unwrap();
                }

                // render and print tree if doing it
                self.print(format!("{prefix}{terminator} ")).unwrap();
            } else if settings.headings && i == 0 {
                let heading = render_heading(db, &padding, max_variable_string_len).join(" ");
                self.println(format!("{}", heading.bold().underline()), LineItem::None)
                    .unwrap();
            }

            // print the device
            let device_string = render_value(
                device,
                db,
                &padding,
                settings,
                max_variable_string_len,
                device.is_disconnected(),
            )
            .join(" ");
            self.println(&device_string, LineItem::Device(device.port_path()))
                .unwrap();

            // print the configurations
            if let Some(extra) = device.extra.as_ref() {
                if settings.verbosity >= 1 || device.is_expanded() {
                    // generate extra blocks if not passed and drop icons if not supported by encoding
                    let blocks = generate_extra_blocks(extra, settings);
                    let num = device
                        .devices
                        .as_ref()
                        .map_or(0, |d| d.iter().filter(|d| !d.is_hidden()).count());

                    // pass branch length as number of configurations for this device plus devices still to print
                    self.print_configurations(
                        device,
                        &extra.configurations,
                        (&blocks.0, &blocks.1, &blocks.2),
                        settings,
                        &generate_tree_data(tree, extra.configurations.len() + num, i, settings),
                    );
                }
            } else if settings.verbosity >= 1 {
                log::warn!(
                    "Unable to print verbose information for {device} because libusb extra data is missing"
                )
            }

            if let Some(d) = device.devices.as_ref() {
                // and then walk down devices printing them too
                self.print_devices(
                    d,
                    db,
                    settings,
                    &generate_tree_data(
                        tree,
                        d.iter().filter(|dd| !dd.is_hidden()).count(),
                        i,
                        settings,
                    ),
                    &padding,
                );
            }
        }
    }

    /// Print [`SystemProfile`] [`Bus`] and [`Device`] information
    pub fn print_sp_usb(&mut self, sp_usb: &SystemProfile, settings: &PrintSettings) {
        let mut bb = settings
            .bus_blocks
            .to_owned()
            .unwrap_or(Block::<BusBlocks, Bus>::default_blocks(settings.more));
        let mut db = settings
            .device_blocks
            .to_owned()
            .unwrap_or(if settings.more {
                DeviceBlocks::default_blocks(true)
            } else if settings.tree {
                DeviceBlocks::default_device_tree_blocks()
            } else {
                DeviceBlocks::default_blocks(false)
            });

        // remove icon blocks if not supported by encoding
        match settings.icon_when {
            IconWhen::Never | IconWhen::Auto if settings.icons.is_none() => {
                bb.retain(|b| !b.is_icon());
                db.retain(|b| !b.is_icon());
            }
            IconWhen::Auto if settings.encoding == Encoding::Glyphs => (),
            IconWhen::Always => {
                if settings.icons.is_none() {
                    log::warn!(
                        "{:?} blocks requested but no icons provided",
                        settings.icon_when
                    );
                }
            }
            _ => {
                settings.icon_when.retain(&sp_usb.buses, &mut bb, settings);
                sp_usb.buses.iter().for_each(|bo| {
                    bo.devices
                        .iter()
                        .for_each(|b| settings.icon_when.retain(b, &mut db, settings));
                });
            }
        }

        let base_tree = TreeData {
            ..Default::default()
        };

        let mut pad: HashMap<BusBlocks, usize> = if !settings.no_padding {
            BusBlocks::generate_padding(&sp_usb.buses.iter().collect::<Vec<&Bus>>())
        } else {
            HashMap::new()
        };
        pad.retain(|k, _| bb.contains(k));

        let max_variable_string_len: Option<usize> = if settings.auto_width {
            let variable_lens: Vec<usize> = pad
                .iter()
                .filter(|(k, _)| k.value_is_variable_length())
                .map(|(_, v)| *v)
                .collect();
            auto_max_string_len(&bb, base_tree.depth * 3, &variable_lens, settings)
                .or(settings.max_variable_string_len)
        } else {
            settings.max_variable_string_len
        };

        // if there is a max variable length, adjust padding to this if current > it
        if let Some(ml) = max_variable_string_len.as_ref() {
            for (k, v) in pad.iter_mut() {
                if k.value_is_variable_length() {
                    *v = cmp::min(*v, *ml);
                }
            }
        }

        log::trace!(
            "print system profile with settings: {settings:?}; padding: {pad:?}; tree {base_tree:?}"
        );

        let len = sp_usb.buses.iter().filter(|b| !b.is_hidden()).count();
        for (i, bus) in sp_usb.buses.iter().filter(|b| !b.is_hidden()).enumerate() {
            if settings.tree {
                let mut prefix = base_tree.prefix.to_owned();
                let mut start = settings.icons.as_ref().map_or(
                    icon::get_default_tree_icon(&icon::Icon::TreeBusStart, &settings.encoding),
                    |i| i.get_tree_icon(&icon::Icon::TreeBusStart, &settings.encoding),
                );

                // colour tree
                if let Some(ct) = settings.colours.as_ref() {
                    prefix = ct
                        .tree
                        .map_or(prefix.normal(), |c| prefix.color(c))
                        .to_string();
                    start = ct
                        .tree_bus_start
                        .map_or(start.normal(), |c| start.color(c))
                        .to_string();
                }

                if settings.headings {
                    let heading = render_heading(&bb, &pad, max_variable_string_len).join(" ");
                    // 2 spaces for bus start icon and space to info
                    self.println(
                        format!("{:>spaces$}{}", "", heading.bold().underline(), spaces = 2),
                        LineItem::Bus(i),
                    )
                    .unwrap();
                }

                self.print(format!("{prefix}{start} ")).unwrap()
            } else if settings.headings {
                let heading = render_heading(&bb, &pad, max_variable_string_len).join(" ");
                // 2 spaces for bus start icon and space to info
                self.println(format!("{}", heading.bold().underline()), LineItem::Bus(i))
                    .unwrap();
            }
            self.println(
                render_value(bus, &bb, &pad, settings, max_variable_string_len, false).join(" "),
                LineItem::Bus(i),
            )
            .unwrap();

            if let Some(d) = bus.devices.as_ref() {
                let num = d.iter().filter(|d| !d.is_hidden()).count();
                let tree = generate_tree_data(&base_tree, num, i, settings);
                let mut padding = if !settings.no_padding {
                    // if tree, generate padding for only local devices
                    // otherwise we need it for all device as flattened
                    if settings.tree {
                        let devices = d
                            .iter()
                            .filter(|d| !d.is_hidden())
                            .collect::<Vec<&Device>>();
                        DeviceBlocks::generate_padding(&devices)
                    } else {
                        let devices = bus
                            .flattened_devices()
                            .into_iter()
                            .filter(|d| !d.is_hidden())
                            .collect::<Vec<&Device>>();
                        DeviceBlocks::generate_padding(&devices)
                    }
                } else {
                    HashMap::new()
                };
                padding.retain(|k, _| db.contains(k));

                // and then walk down devices printing them too
                self.print_devices(d, &db, settings, &tree, &padding);
            }

            // separate bus groups with line
            if i + 1 != len {
                self.println("", LineItem::None).unwrap();
            }
        }
    }

    /// Print `devices` [`Device`] references without looking down each device's devices!
    pub fn print_flattened_devices(&mut self, devices: &[&Device], settings: &PrintSettings) {
        let mut db = settings
            .device_blocks
            .to_owned()
            .unwrap_or(DeviceBlocks::default_blocks(settings.more));

        // remove icon blocks if not supported
        match settings.icon_when {
            IconWhen::Never | IconWhen::Auto if settings.icons.is_none() => {
                db.retain(|b| !b.is_icon());
            }
            IconWhen::Auto if settings.encoding == Encoding::Glyphs => (),
            IconWhen::Always => {
                if settings.icons.is_none() {
                    log::warn!(
                        "{:?} blocks requested but no icons provided",
                        settings.icon_when
                    );
                }
            }
            _ => settings.icon_when.retain_ref(devices, &mut db, settings),
        }

        let mut pad = if !settings.no_padding {
            DeviceBlocks::generate_padding(devices)
        } else {
            HashMap::new()
        };
        pad.retain(|k, _| db.contains(k));
        log::trace!("Flattened devices padding {pad:?}");

        let max_variable_string_len: Option<usize> = if settings.auto_width {
            let variable_lens: Vec<usize> = pad
                .iter()
                .filter(|(k, _)| k.value_is_variable_length())
                .map(|(_, v)| *v)
                .collect();
            auto_max_string_len(&db, 0, &variable_lens, settings)
                .or(settings.max_variable_string_len)
        } else {
            settings.max_variable_string_len
        };

        // if there is a max variable length, adjust padding to this if current > it
        if let Some(ml) = max_variable_string_len.as_ref() {
            for (k, v) in pad.iter_mut() {
                if k.value_is_variable_length() {
                    *v = cmp::min(*v, *ml);
                }
            }
        }

        if settings.headings {
            let heading = render_heading(&db, &pad, max_variable_string_len).join(" ");
            println!("{}", heading.bold().underline());
        }

        for (i, device) in devices.iter().enumerate() {
            println!(
                "{}",
                render_value(
                    *device,
                    &db,
                    &pad,
                    settings,
                    max_variable_string_len,
                    device.is_disconnected()
                )
                .join(" ")
            );
            // print the configurations
            if let Some(extra) = device.extra.as_ref() {
                if settings.verbosity >= 1 || device.is_expanded() {
                    let blocks = generate_extra_blocks(extra, settings);

                    // pass branch length as number of configurations for this device plus devices still to print
                    self.print_configurations(
                        device,
                        &extra.configurations,
                        (&blocks.0, &blocks.1, &blocks.2),
                        settings,
                        &generate_tree_data(
                            &Default::default(),
                            extra.configurations.len()
                                + device
                                    .devices
                                    .as_ref()
                                    .map_or(0, |d| d.iter().filter(|d| !d.is_hidden()).count()),
                            i,
                            settings,
                        ),
                    );
                }
            } else if settings.verbosity >= 1 {
                log::warn!(
                    "Unable to print verbose information for {device} because libusb extra data is missing"
                )
            }
        }
    }

    /// A way of printing a reference flattened [`SystemProfile`] rather than hard flatten
    ///
    /// Prints each `&Bus` and tuple pair `Vec<&Device>`
    pub fn print_bus_grouped(
        &mut self,
        bus_devices: Vec<(&Bus, Vec<&Device>)>,
        settings: &PrintSettings,
    ) {
        let bb = settings
            .bus_blocks
            .to_owned()
            .unwrap_or(Block::<BusBlocks, Bus>::default_blocks(settings.more));
        let mut pad: HashMap<BusBlocks, usize> = if !settings.no_padding {
            let buses: Vec<&Bus> = bus_devices.iter().map(|bd| bd.0).collect();
            BusBlocks::generate_padding(&buses)
        } else {
            HashMap::new()
        };
        pad.retain(|k, _| bb.contains(k));

        let max_variable_string_len: Option<usize> = if settings.auto_width {
            let mut variable_lens = pad.clone();
            variable_lens.retain(|k, _| k.value_is_variable_length());
            auto_max_string_len(&bb, 0, &variable_lens.into_values().collect(), settings)
                .or(settings.max_variable_string_len)
        } else {
            settings.max_variable_string_len
        };

        // if there is a max variable length, adjust padding to this if current > it
        if let Some(ml) = max_variable_string_len.as_ref() {
            for (k, v) in pad.iter_mut() {
                if k.value_is_variable_length() {
                    *v = cmp::min(*v, *ml);
                }
            }
        }

        let len = bus_devices.len();
        for (i, (bus, devices)) in bus_devices.into_iter().enumerate() {
            if settings.headings {
                let heading = render_heading(&bb, &pad, max_variable_string_len).join(" ");
                self.println(format!("{}", heading.bold().underline()), LineItem::Bus(i))
                    .unwrap();
            }
            self.println(
                render_value(bus, &bb, &pad, settings, max_variable_string_len, false).join(" "),
                LineItem::Bus(i),
            )
            .unwrap();
            self.print_flattened_devices(&devices, settings);
            // new line for each group
            if i + 1 != len {
                self.println("", LineItem::None).unwrap();
            }
        }
    }
}

/// Mask the `device` serial if it has one using the [`MaskSerial`] method and recursively if `recursive`
pub fn mask_serial(device: &mut Device, hide: &MaskSerial, recursive: bool) {
    if let Some(serial) = device.serial_num.as_mut() {
        *serial = match hide {
            MaskSerial::Hide => serial.chars().map(|_| '*').collect::<String>(),
            MaskSerial::Scramble => serial
                .chars()
                .map(|_| {
                    serial
                        .chars()
                        .nth(fastrand::usize(0..serial.len()))
                        .unwrap_or('*')
                })
                .collect::<String>(),
            MaskSerial::Replace => serial
                .chars()
                .map(|_| fastrand::alphanumeric())
                .collect::<String>()
                .to_uppercase(),
        };
    }

    if recursive {
        device
            .devices
            .iter_mut()
            .for_each(|dd| dd.iter_mut().for_each(|d| mask_serial(d, hide, recursive)));
    }
}

/// Main cyme bin prepare for printing function - changes mutable `sp_usb` with requested `filter` and sort in `settings`
pub fn prepare(sp_usb: &mut SystemProfile, filter: Option<&Filter>, settings: &PrintSettings) {
    // if not printing tree, hard flatten now before filtering as filter will retain non-matching parents with matching devices in tree
    // flattening now will also mean hubs will be removed when listing if `hide_hubs` because they will appear empty and sorting will be in bus -> device order rather than tree position
    log::debug!("Running prepare pre-printing");
    if !settings.tree && !matches!(settings.print_mode, PrintMode::Dynamic) {
        log::debug!("Flattening SPUSBDataType");
        sp_usb.into_flattened();
    }

    // do the filter if present; will keep parents of matched devices even if they do not match
    log::debug!("Filtering with {filter:?}");
    if let Some(filter) = filter {
        if matches!(settings.print_mode, PrintMode::Dynamic) {
            filter.hide_buses(&mut sp_usb.buses);
        } else {
            filter.retain_buses(&mut sp_usb.buses);
        }
    }

    // sort device tree based on sort option
    log::debug!("Sorting with {:?}", settings.sort_devices);
    settings.sort_devices.sort_buses(&mut sp_usb.buses);

    // sort the buses if asked and not already sorted
    if settings.sort_buses && matches!(settings.sort_devices, Sort::NoSort) {
        log::debug!("Sorting buses by bus number");
        sp_usb.buses.sort_by_key(|d| d.get_bus_number());
    }

    // hide serials Recursively
    if let Some(hide) = settings.mask_serials.as_ref() {
        log::debug!("Masking serials with {hide:?}");
        for bus in &mut sp_usb.buses {
            bus.devices.iter_mut().for_each(|devices| {
                for device in devices {
                    mask_serial(device, hide, true);
                }
            });
        }
    }

    log::trace!("sp_usb data post filter and bus sort\n\r{sp_usb:#}");
}

/// Main cyme bin print function
pub fn print(sp_usb: &SystemProfile, settings: &PrintSettings) {
    log::trace!("Printing with {settings:?}");
    let mut dw = DisplayWriter::default();

    match settings.color_when {
        ColorWhen::Always => colored::control::set_override(true),
        ColorWhen::Never => colored::control::set_override(false),
        ColorWhen::Auto => colored::control::unset_override(),
    }

    if settings.tree || settings.group_devices == Group::Bus {
        if settings.json {
            println!("{}", serde_json::to_string_pretty(&sp_usb).unwrap());
        } else {
            dw.print_sp_usb(sp_usb, settings);
        }
    } else {
        // get a list of all devices
        let devs = sp_usb.flattened_devices();

        if settings.json {
            println!("{}", serde_json::to_string_pretty(&devs).unwrap());
        } else {
            dw.print_flattened_devices(&devs, settings);
        }
    }
}
