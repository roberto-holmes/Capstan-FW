use crate::services::{BasService, DisService, HidService};
use crate::{Irqs, services};
use bt_hci::event::EncryptionChangeV1;
use bt_hci::param::EncryptionEnabledLevel;
use defmt::*;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32_wpan::bluetooth::gap::{AdvData, AdvParams, AdvType, GapEvent, OwnAddressType};
use embassy_stm32_wpan::bluetooth::gap_init::GapInitParams;
use embassy_stm32_wpan::bluetooth::gatt::{CccdValue, GattServer, is_cccd_handle, is_value_handle};
use embassy_stm32_wpan::bluetooth::security::{
    IdentityAddressType, IoCapability, SecureConnectionsSupport, SecurityEvent, SecurityManager,
    SecurityParams,
};
use embassy_stm32_wpan::bluetooth::{EventBuffer, HCI};
use embassy_stm32_wpan::{Platform, erase_bond_nvm_flash, new_platform, set_nvm_base_address};
use panic_probe as _;
use stm32wb_hci::aci::AciEvent;
use stm32wb_hci::aci::att::AttExchangeMtuRespEvent;
use stm32wb_hci::aci::gap::GapPairingCompleteEvent;
use stm32wb_hci::aci::values::PairingStatus;
use stm32wb_hci::event::BleEvent;
use stm32wb_hci::wire::OrUnknown;

const ADVERTISING_NAME: &'static str = "HID_ADV";
const DEVICE_NAME: &'static [u8] = b"Capstan";

// TODO: Find an OUI or falsify a RandomStatic address from the chip's UID
const OUI: [u8; 3] = [0x00, 0x80, 0xe1]; // STMicroelectronics

const GAP_APPEARANCE_HID_MOUSE: u16 = 0x03c2;

// ── GATT UUIDs (Bluetooth SIG assigned) ──────────────────────────────────────

// HID Control Point Characteristic - Bluetooth HIDS v1.0 2.11
const HID_CONTROL_POINT_SUSPEND: u8 = 0x00;
const HID_CONTROL_POINT_EXIT_SUSPEND: u8 = 0x01;

/// Erase bond NVM **before** BLE init. Set `true` once to clear stale bonds, flash, then `false`.
const ERASE_BONDS_ON_BOOT: bool = false;

// HID Over GATT Profile
// https://www.bluetooth.com/specifications/specs/html/?src=hogp_v1-2_1777405843/HOGP_v1.2/out/en/index-en.html
#[allow(dead_code)]
struct HogpState {
    pub hid: HidService,
    pub dis: DisService,
    pub bas: BasService,

    pub conn_handle: Option<u16>,
}

impl HogpState {
    fn reset_notifications(&mut self) {
        self.hid.notifications_enabled = false;
        self.bas.notifications_enabled = false;
    }
}

fn get_addr() -> [u8; 6] {
    let uid = embassy_stm32::uid::uid();
    // TODO: Figure out a way to hash the UID to avoid collisions
    let bd_addr = [
        uid[0] ^ uid[3] ^ uid[6] ^ uid[9],  // Last byte
        uid[1] ^ uid[4] ^ uid[7] ^ uid[10], // Penultimate
        uid[2] ^ uid[5] ^ uid[8] ^ uid[11], // Antepenultimate
        OUI[2],
        OUI[1],
        OUI[0],
    ];
    warn!("OUI: {:x}", OUI);
    warn!("UID: {:x}", uid);
    // [10, 0, 46, 0, 3, 50, 36, 43, 42, 34, 33, 20]
    //  10  0  'F' 0  3  'P' '6' 'C' 'B' '4' '3' 20
    bd_addr
}

#[embassy_executor::task]
pub async fn ble_runner_task(platform: &'static Platform) {
    platform.run_ble().await
}

pub struct BLE<'a> {
    state: HogpState,

    security: SecurityManager,

    // Advertising
    adv_params: AdvParams,
    adv_data: AdvData,

    gatt: GattServer,
    ble: HCI<'a, embassy_stm32_wpan::bluetooth::Normal>,
}

impl<'a> BLE<'a> {
    pub async fn new(spawner: &Spawner) -> Self {
        // Configure the last 8KB page of BANK_2 for bond storage.
        // Must be called before new_platform! triggers BleStack_Init.
        set_nvm_base_address(0x081F_E000);
        // Following ble_bonding example
        // TODO: How do we signal that we want to erase the flash?
        // This needs to run after set_nvm_base_address but before new_platform
        if ERASE_BONDS_ON_BOOT {
            if erase_bond_nvm_flash() {
                info!("Bond NVM flash page erased — next boot should not restore bonds");
            } else {
                warn!("Bond NVM flash erase failed");
            }
        }

        let (platform, runtime) = new_platform!(8);

        spawner.spawn(ble_runner_task(platform).expect("Failed to spawn BLE runner"));

        let gap_params = GapInitParams {
            device_name: DEVICE_NAME,
            appearance: GAP_APPEARANCE_HID_MOUSE,
            address_type: embassy_stm32_wpan::bluetooth::gap_init::AddressType::Public,
            bd_addr: get_addr(),
            ..Default::default()
        };
        let mut ble = HCI::new_with_gap_params(platform, runtime, Irqs, gap_params)
            .await
            .expect("BLE init failed");

        // ===== Configure Security =====
        let mut security = ble.security_manager();

        // Configure security parameters:
        // - Enable bonding (store keys)
        // - Require MITM protection (passkey entry or numeric comparison)
        // - Support Secure Connections (LE Secure Connections pairing)
        let security_params = SecurityParams::new()
            .with_bonding(true)
            .with_mitm_protection(true)
            .with_secure_connections(SecureConnectionsSupport::Optional)
            .with_key_size_range(8, 16)
            .with_io_capability(IoCapability::DisplayYesNo)
            .with_identity_address_type(IdentityAddressType::Public);

        security
            .set_authentication_requirements(security_params)
            .expect("Failed to set security parameters");
        info!("Security configured: Bonding + MITM + SC");

        // Enable address resolution (for bonded devices using RPA)
        // TODO: Necessary?
        security
            .set_address_resolution_enable(true)
            .expect("Failed to enable address resolution");

        // ST: initialize whitelist when bonding is enabled.
        security
            .configure_filter_accept_list()
            .expect("configure_filter_accept_list");

        if ERASE_BONDS_ON_BOOT {
            security
                .clear_security_database()
                .expect("clear_security_database");
            info!("Bond RAM database cleared (ERASE_BONDS_ON_BOOT)");
        }

        // Cold boot with bonds in NVM: program FAL + resolving list (ST privacy flow).
        match security.configure_filter_and_resolving_list() {
            Ok(count) => {
                if count > 0 {
                    info!("Bond lists configured at boot ({} peer(s))", count);
                }
            }
            Err(e) => warn!(
                "configure_filter_and_resolving_list at boot failed: {:?}",
                e
            ),
        }

        // Dump the controller's view of the resolving list (debug). Useful for verifying that
        // the bond's IRK made it into the LL resolving list — if `peer_rpa` here matches the
        // identity address (instead of being a valid RPA), the LL has `peer_irk = 0` and will
        // silently drop incoming CONNECT_INDs from a bonded RPA-using peer like iOS.
        security.log_resolving_list_diagnostics(None);

        embassy_futures::yield_now().await;

        // ── Build GATT server ────────────────────────────────────────────────────
        let mut gatt = ble.gatt_server();

        let hid = HidService::setup(&mut gatt);
        let dis = DisService::setup(&mut gatt, 0x02, 0xdead, 0xcafe, 0x01);
        let bas = BasService::setup(&mut gatt, 100);

        let mut adv_data = AdvData::new();
        adv_data.add_flags(0x06).unwrap();
        adv_data.add_name(ADVERTISING_NAME).unwrap();
        adv_data
            .add_service_uuid_16(services::HID_SERVICE_UUID)
            .unwrap();

        let adv_params = AdvParams {
            interval_min: 0x0050, // 50 ms
            interval_max: 0x0064, // 62.5 ms
            adv_type: AdvType::ConnectableUndirected,
            own_addr_type: OwnAddressType::Public,
            ..AdvParams::default()
        };

        BLE {
            state: HogpState {
                hid,
                dis,
                bas,
                conn_handle: None,
            },
            security,
            adv_data,
            adv_params,
            ble,
            gatt,
        }
    }

    pub async fn advertise(&mut self) {
        // ── Advertising ──────────────────────────────────────────────────────────
        self.ble
            .start_advertising(self.adv_params.clone(), self.adv_data.clone(), None)
            .await
            .expect("Failed to start advertising");
        info!("Advertising as '{}'", ADVERTISING_NAME);
    }

    pub fn erase_bond() {
        // TODO: How do we do this?
    }

    pub async fn read_event<'b>(&mut self, event_buffer: &'b mut EventBuffer) -> BleEvent<'b> {
        self.ble.read_event(event_buffer).await
    }

    pub async fn process_event(&mut self, event: BleEvent<'_>) {
        // ── GAP events ────────────────────────────────────────────────
        if let Some(gap_event) = self.ble.process_event(&event) {
            self.process_event_gap(gap_event).await;
        }
        // ── Security events ───────────────────────────────────────────
        if let Some(security_event) = self.ble.process_security_event(&event) {
            self.process_event_security(security_event).await;
        }
        // ── GATT events ───────────────────────────────────────────────
        self.process_event_gatt(event).await;
    }

    async fn process_event_gap(&mut self, event: GapEvent) {
        match event {
            GapEvent::Connected(conn) => {
                info!("Connected: 0x{:04X}", conn.handle.0);
                self.state.conn_handle = Some(conn.handle.0);
                self.state.reset_notifications();
            }
            GapEvent::Disconnected { handle, reason } => {
                info!(
                    "Disconnected: 0x{:04X}, reason 0x{:02X} ({})",
                    handle.0,
                    reason.as_u8(),
                    Display2Format(&reason)
                );
                self.state.conn_handle = None;
                self.state.reset_notifications();
                self.advertise().await;
            }
            _ => {
                warn!("Unknown GAP event {:?}", event);
            }
        }
    }

    async fn process_event_security(&mut self, event: SecurityEvent) {
        match event {
            SecurityEvent::PairingComplete {
                conn_handle,
                status,
                reason,
            } => {
                info!("=== PAIRING COMPLETE ===");
                info!("  Connection: 0x{:04X}", conn_handle);
                info!("  Status: {:?}, Reason: 0x{:02X}", status, reason);
                if matches!(
                    status,
                    embassy_stm32_wpan::bluetooth::security::PairingStatus::Success
                ) {
                    info!("  Device is now bonded and can access secure characteristics");
                }
            }
            SecurityEvent::PasskeyRequest { conn_handle } => {
                info!("=== PASSKEY REQUEST ===");
                info!("  Connection: 0x{:04X}", conn_handle);

                // For demo: fixed passkey.
                let passkey: u32 = 123456;
                info!("  Passkey: {:06}", passkey);
                info!("  Enter this passkey on your phone/device!");

                if let Err(e) = self.security.pass_key_response(conn_handle, passkey) {
                    error!("Failed to send passkey response: {:?}", e);
                }
            }
            SecurityEvent::NumericComparisonRequest {
                conn_handle,
                numeric_value,
            } => {
                info!("=== NUMERIC COMPARISON ===");
                info!("  Connection: 0x{:04X}", conn_handle);
                info!("  Displayed value: {:06}", numeric_value);

                // Auto-confirm for this example.
                let confirm = true;
                info!("  Auto-confirming: {}", if confirm { "YES" } else { "NO" });

                if let Err(e) = self
                    .security
                    .numeric_comparison_response(conn_handle, confirm)
                {
                    error!("Failed to send numeric comparison response: {:?}", e);
                }
            }
            SecurityEvent::BondLost { conn_handle } => {
                info!("=== BOND LOST ===");
                info!("  Connection: 0x{:04X}", conn_handle);
                if let Err(e) = self.security.allow_rebond(conn_handle) {
                    warn!("Failed to allow rebond: {:?}", e);
                }
            }
            SecurityEvent::PairingRequest { .. } => {}
            SecurityEvent::AuthorizationRequest { conn_handle } => {
                info!("=== AUTHORIZATION REQUEST ===");
                info!("  Connection: 0x{:04X}", conn_handle);
            }
            // SecurityEvent::PeripheralSecurityInitiated => {
            //     info!("=== PERIPHERAL SECURITY INITIATED ===");
            // }
            SecurityEvent::AddressNotResolved { conn_handle } => {
                warn!("=== ADDRESS NOT RESOLVED ===");
                warn!("  Connection: 0x{:04X}", conn_handle);
            }
            SecurityEvent::KeypressNotification {
                conn_handle,
                notification_type,
            } => {
                info!("=== KEYPRESS NOTIFICATION ===");
                info!("  Connection: 0x{:04X}", conn_handle);
                info!("  Notification: {:?}", notification_type);
            }
        }
    }

    async fn process_event_gatt(&mut self, event: BleEvent<'_>) {
        match event {
            // ── Numeric comparison (LE SC path — the normal iOS path) ───────
            // Both sides computed the same 6-digit value from the ECDH
            // exchange. We auto-confirm; iOS shows a popup asking the user
            // to confirm the same value shown here.
            BleEvent::Vendor(AciEvent::GapNumericComparisonValue(ev)) => {
                info!("");
                info!("╔══════════════════════════════════╗");
                info!("║  CONFIRM ON PHONE: {:06}        ║", ev.numeric_value);
                info!("╚══════════════════════════════════╝");
                info!("");
                if let Err(e) = self
                    .security
                    .numeric_comparison_response(ev.connection_handle.0, true)
                {
                    error!("numeric_comparison_response error: {:?}", e);
                }
            }

            // ── Pairing result ──────────────────────────────────────────────
            BleEvent::Vendor(AciEvent::GapPairingComplete(GapPairingCompleteEvent {
                connection_handle: conn_handle,
                status,
                reason,
            })) => match status {
                OrUnknown::Known(PairingStatus::Success) => {
                    info!(
                        "PAIRING COMPLETE: handle=0x{:04X} — encrypted and bonded",
                        conn_handle.0
                    );
                    self.security.log_bonded_devices();
                }
                OrUnknown::Known(PairingStatus::SmpTimeout) => {
                    warn!(
                        "Pairing timed out: handle=0x{:04X} reason={:?}",
                        conn_handle.0, reason
                    );
                }
                OrUnknown::Known(PairingStatus::PairingFailed) | OrUnknown::Unknown(_) => {
                    warn!(
                        "Pairing failed: handle=0x{:04X} reason={:?} — wrong code entered?",
                        conn_handle.0, reason
                    );
                }
                OrUnknown::Known(PairingStatus::EncryptionFailed) => {
                    warn!(
                        "Encryption failed: handle=0x{:04X} reason={:?}",
                        conn_handle.0, reason
                    );
                }
            },

            // ── Encryption state (also fires on reconnect with stored bond) ─
            BleEvent::Core(bt_hci::event::Event::EncryptionChangeV1(EncryptionChangeV1 {
                handle,
                enabled,
                ..
            })) => match enabled {
                EncryptionEnabledLevel::OnE0OrAesCcm | EncryptionEnabledLevel::OnAesCcm => {
                    info!("Link encrypted: handle=0x{:04X}", handle.raw());
                }
                EncryptionEnabledLevel::Off => {
                    warn!("Encryption disabled: handle=0x{:04X}", handle.raw());
                }
            },

            // ── Bond lost / LTK mismatch: allow re-pair and restart open advertising ─
            BleEvent::Vendor(AciEvent::GapBondLost(_)) => {
                info!("Bond lost — allowing rebond and restarting advertising");
                let conn_handle = self
                    .ble
                    .connections()
                    .iter()
                    .next()
                    .map(|c| c.handle.raw())
                    .unwrap_or(0);
                let _ = self.security.allow_rebond(conn_handle);
                if self.ble.is_advertising() {
                    let _ = self.ble.stop_advertising().await;
                }
                self.advertise().await;
            }

            // ── Authenticated write received from bonded peer ─────────────
            BleEvent::Vendor(AciEvent::GattAttributeModified(attr)) => {
                info!(
                    "Authenticated write: conn=0x{:04X} attr=0x{:04X} data={:?}",
                    attr.connection_handle, attr.attr_handle, attr.attr_data
                );
                // CCCD write → enable/disable notifications
                if is_cccd_handle(self.state.hid.report_handle(), attr.attr_handle) {
                    let cccd = CccdValue::from_bytes(attr.attr_data);
                    // TODO: Separate notifications
                    self.state.hid.notifications_enabled = cccd.notifications;
                    info!(
                        "HID notifications {}",
                        if cccd.notifications {
                            "ENABLED"
                        } else {
                            "DISABLED"
                        }
                    );
                } else if is_cccd_handle(self.state.bas.batt_level_handle(), attr.attr_handle) {
                    let cccd = CccdValue::from_bytes(attr.attr_data);
                    self.state.bas.notifications_enabled = cccd.notifications;
                    info!(
                        "BAS notifications {}",
                        if cccd.notifications {
                            "ENABLED"
                        } else {
                            "DISABLED"
                        }
                    );
                } else if is_value_handle(self.state.hid.control_point_handle(), attr.attr_handle) {
                    if attr.attr_data.first().copied() == Some(HID_CONTROL_POINT_SUSPEND) {
                        info!("Suspend device");
                    } else if attr.attr_data.first().copied()
                        == Some(HID_CONTROL_POINT_EXIT_SUSPEND)
                    {
                        info!("Wake device");
                    } else {
                        info!("Unknown HID Control Point command: {:?}", attr.attr_data);
                    }
                }
            }
            BleEvent::Vendor(AciEvent::AttExchangeMtuResp(AttExchangeMtuRespEvent {
                connection_handle,
                server_rx_mtu,
            })) => {
                if let Some(conn) = self.ble.get_connection_mut(connection_handle) {
                    conn.update_mtu(server_rx_mtu);
                }
            }
            // BleEvent::LeDataLengthChangeEvent(_) => info!("Data Length Change event"),
            // BleEvent::LeEnhancedConnectionComplete(_) => {
            //     info!("Enhanced Connection Complete event")
            // }
            gatt_event => {
                warn!("Unknown GATT event {:?}", gatt_event);
            }
        }
    }

    pub async fn update_battery(&mut self) {
        if let Some(conn) = self.state.conn_handle {
            if let Err(e) = self.state.bas.update(&mut self.gatt, conn) {
                error!("Battery notify failed: {:?}", e);
            }
        }
    }

    pub async fn scroll(&mut self, amount: i8) {
        if let Some(conn) = self.state.conn_handle {
            if let Err(e) = self.state.hid.scroll(&mut self.gatt, conn, amount) {
                error!("HID report notify failed: {:?}", e);
            }
        }
    }
}
