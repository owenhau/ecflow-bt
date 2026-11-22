//! Hand-written subset of `pr705.proto` (`DisplayPropertyUpload`), field numbers taken
//! from the River 3 descriptor in rabits/ha-ef-ble. Unknown fields are skipped by prost.
//! Hand-written so no `protoc` is needed at build time.

#[derive(Clone, PartialEq, prost::Message)]
pub struct DisplayPropertyUpload {
    #[prost(uint32, optional, tag = "1")]
    pub errcode: Option<u32>,
    #[prost(float, optional, tag = "3")]
    pub pow_in_sum_w: Option<f32>,
    #[prost(float, optional, tag = "4")]
    pub pow_out_sum_w: Option<f32>,
    #[prost(bool, optional, tag = "7")]
    pub energy_backup_en: Option<bool>,
    #[prost(float, optional, tag = "9")]
    pub pow_get_qcusb1: Option<f32>,
    #[prost(float, optional, tag = "11")]
    pub pow_get_typec1: Option<f32>,
    #[prost(uint32, optional, tag = "33")]
    pub flow_info_12v: Option<u32>,
    #[prost(float, optional, tag = "37")]
    pub pow_get_12v: Option<f32>,
    #[prost(float, optional, tag = "54")]
    pub pow_get_ac_in: Option<f32>,
    #[prost(float, optional, tag = "158")]
    pub pow_get_bms: Option<f32>,
    #[prost(bool, optional, tag = "202")]
    pub plug_in_info_ac_charger_flag: Option<bool>,
    #[prost(uint32, optional, tag = "209")]
    pub plug_in_info_ac_in_chg_pow_max: Option<u32>,
    #[prost(float, optional, tag = "242")]
    pub bms_batt_soc: Option<f32>,
    #[prost(int32, optional, tag = "259")]
    pub bms_max_cell_temp: Option<i32>,
    #[prost(float, optional, tag = "262")]
    pub cms_batt_soc: Option<f32>,
    #[prost(uint32, optional, tag = "268")]
    pub cms_dsg_rem_time: Option<u32>,
    #[prost(uint32, optional, tag = "269")]
    pub cms_chg_rem_time: Option<u32>,
    #[prost(uint32, optional, tag = "270")]
    pub cms_max_chg_soc: Option<u32>,
    #[prost(uint32, optional, tag = "271")]
    pub cms_min_dsg_soc: Option<u32>,
    #[prost(uint32, optional, tag = "357")]
    pub led_mode: Option<u32>,
    #[prost(float, optional, tag = "361")]
    pub pow_get_pv: Option<f32>,
    #[prost(uint32, optional, tag = "367")]
    pub flow_info_ac_out: Option<u32>,
    #[prost(float, optional, tag = "368")]
    pub pow_get_ac_out: Option<f32>,
    #[prost(bool, optional, tag = "426")]
    pub plug_in_info_dcp_in_flag: Option<bool>,
}

/// `SysUTCSync` from utc_sys.proto.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SysUtcSync {
    #[prost(uint32, optional, tag = "1")]
    pub sys_utc_time: Option<u32>,
}

/// Subset of `ConfigWrite` (pr705.proto): the settings this tool can change.
/// Field numbers come from the reference `pr705_pb2` descriptor; unset fields are not
/// encoded, so a write changes exactly one setting.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ConfigWrite {
    #[prost(bool, optional, tag = "18")]
    pub cfg_dc_12v_out_open: Option<bool>,
    #[prost(uint32, optional, tag = "33")]
    pub cfg_max_chg_soc: Option<u32>,
    #[prost(uint32, optional, tag = "34")]
    pub cfg_min_dsg_soc: Option<u32>,
    #[prost(bool, optional, tag = "76")]
    pub cfg_ac_out_open: Option<bool>,
    #[prost(uint32, optional, tag = "89")]
    pub cfg_led_mode: Option<u32>,
}

impl ConfigWrite {
    /// `max_chg_soc=85 led_mode=2`-style summary of the fields that are set.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(v) = self.cfg_max_chg_soc {
            parts.push(format!("max_chg_soc={v}"));
        }
        if let Some(v) = self.cfg_min_dsg_soc {
            parts.push(format!("min_dsg_soc={v}"));
        }
        if let Some(v) = self.cfg_led_mode {
            parts.push(format!("led_mode={v}"));
        }
        if let Some(v) = self.cfg_dc_12v_out_open {
            parts.push(format!("dc_12v_out_open={v}"));
        }
        if let Some(v) = self.cfg_ac_out_open {
            parts.push(format!("ac_out_open={v}"));
        }
        if parts.is_empty() {
            "(no known fields set)".into()
        } else {
            parts.join(" ")
        }
    }
}

/// Packet addressing for configuration writes (as used by the reference implementation).
pub const CONFIG_WRITE_SRC: u8 = 0x20;
pub const CONFIG_WRITE_DST: u8 = 0x02;
pub const CONFIG_WRITE_CMD_SET: u8 = 0xFE;
pub const CONFIG_WRITE_CMD_ID: u8 = 0x11;
pub const CONFIG_WRITE_PACKET_VERSION: u8 = 0x13;
