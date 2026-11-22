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
