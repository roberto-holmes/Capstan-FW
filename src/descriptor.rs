use defmt::warn;
use usbd_hid::descriptor::{
    AsInputReport, BufferOverflow, SerializedDescriptor, gen_hid_descriptor,
};

// Refer to usbd_hid::descriptor::MouseReport as a starting point

/// CapstanReport describes a report and its companion descriptor than can be used
/// to send capstan movements and button presses to a host.
#[gen_hid_descriptor(
    (collection = APPLICATION, usage_page = GENERIC_DESKTOP, usage = MOUSE) = {
        (collection = LOGICAL, usage_page = GENERIC_DESKTOP, usage = MOUSE) = {
            (collection = PHYSICAL, report_id = 0x01, usage = POINTER) = {
                (usage_page = BUTTON, usage_min = BUTTON_1, usage_max = BUTTON_8) = {
                    #[packed_bits = 8] #[item_settings(data,variable,absolute)] buttons=input;
                };
                (usage_page = GENERIC_DESKTOP,) = {
                    // From -127 to 127
                    (usage = X, logical_min = 0x81, logical_max = 0x7F) = {
                        #[item_settings(data,variable,relative)] x=input;
                    };
                    (usage = Y, logical_min = 0x81, logical_max = 0x7F) = {
                        #[item_settings(data,variable,relative)] y=input;
                    };
                };
                (collection = LOGICAL,) = {
                    // Resolution multiplier
                    (report_id = 0x02, usage = 0x48, report_id = 0x02, logical_min = 0, logical_max = 1, physical_min = 1, physical_max = 16) = {
                        #[item_settings(const,variable,absolute)] y_res=feature;
                    };
                    (report_id = 0x01, usage = WHEEL, physical_min = 0, physical_max = 0, logical_min = 0x81, logical_max = 0x7F) = {
                       #[item_settings(data,variable,relative)] wheel=input;
                    };
                };
                (collection = LOGICAL,) = {
                    // Resolution multiplier
                    (report_id = 0x02, usage = 0x48, logical_min = 0, logical_max = 1, physical_min = 1, physical_max = 16) = {
                        #[item_settings(const,variable,absolute)] y_res=feature;
                    };
                    (report_id = 0x01, usage_page = CONSUMER,) = {
                        (usage = AC_PAN,) = {
                            #[item_settings(data,variable,relative)] pan=input;
                        };
                    };
                };
            };
        };
    }
)]
pub struct CapstanReport {
    pub buttons: u8,
    pub x: i8,
    pub y: i8,
    pub wheel: i8, // Scroll down (negative) or up (positive) this many units
    pub pan: i8,   // Scroll left (negative) or right (positive) this many units
    pub y_res: u8,
    pub x_res: u8,
}

impl CapstanReport {
    pub fn new(scroll: i8) -> Self {
        Self {
            buttons: 0,
            x: 0,
            y: 0,

            y_res: 1, // TODO: How do we get this number to actually change the resolution
            // wheel: 0,
            wheel: scroll,
            x_res: 1,
            pan: 0,
            // pan: scroll,
        }
    }
    pub fn serialise_features(
        &self,
        buf: &mut [u8],
    ) -> Result<usize, usbd_hid::descriptor::BufferOverflow> {
        if buf.len() < 2 {
            return Err(BufferOverflow);
        }
        buf[0] = self.y_res;
        buf[1] = self.x_res;
        Ok(2)
    }
}

impl AsInputReport for CapstanReport {
    fn serialize(&self, buf: &mut [u8]) -> Result<usize, usbd_hid::descriptor::BufferOverflow> {
        if buf.len() < 6 {
            warn!("Trying to serialise into a buffer of {} bytes", buf.len());
            return Err(BufferOverflow);
        }
        buf[0] = 0x01; // Report ID
        buf[1] = self.buttons;
        buf[2] = self.x as u8;
        buf[3] = self.y as u8;
        buf[4] = self.wheel as u8;
        buf[5] = self.pan as u8;
        Ok(6)
    }
}
