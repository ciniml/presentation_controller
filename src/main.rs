
#[cfg(target_os="espidf")]
mod espidf_imports {
    pub use embedded_svc::{wifi::*, timer::*, http::client::Response};
    pub use esp_idf_hal::prelude::*;
    pub use esp_idf_hal::delay;
    pub use esp_idf_hal::gpio;
    pub use esp_idf_hal::peripheral;
    pub use esp_idf_svc::{netif::*, wifi::*, timer::*, nvs::*, eventloop::EspSystemEventLoop, systime::EspSystemTime};
    pub use esp_idf_sys as _; // If using the `binstart` feature of `esp-idf-sys`, always keep this module imported
    pub use embedded_svc::http::Headers;
}
use byteorder::{LittleEndian, ReadBytesExt};
use chrono::{DateTime, FixedOffset, Utc};
use embedded_graphics::{pixelcolor::Rgb888, prelude::RgbColor};
use esp_idf_svc::sntp;
#[cfg(target_os="espidf")]
use espidf_imports::*;

use std::{ffi::CStr, fmt::Write, net::UdpSocket, str::FromStr, sync::{Arc, Mutex}, time::Duration};

use anyhow::anyhow;
use embedded_io::blocking::Read;

use rand::prelude::*;

use lgfx::{self, textdatum_top_left, ColorRgb332, ColorRgb888, DrawString, Screen};
use heapless::Vec;

use lgfx::{DrawImage, DrawPrimitives, Gfx};
use uuid::Uuid;

use crate::lgfx::{EpdMode, DrawChars, FontManupulation, LgfxDisplay};

mod config;
use config::*;

#[derive(Default, Debug)]
struct Config {
    wifi_ssid: heapless::String<32>,
    wifi_password: heapless::String<64>,
    device_id: Uuid,
    appliance_id: Uuid,
    access_token: heapless::String<128>,
}

static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

#[cfg(target_os="espidf")]
fn init_config() {
    let mut config = Config::default();
    config.wifi_ssid = heapless::String::from_str(env!("WIFI_SSID")).unwrap();
    config.wifi_password = heapless::String::from_str(env!("WIFI_PASS")).unwrap();
    log::info!("init_config {:?}", config);
    *CONFIG.lock().unwrap() = Some(config);
}
#[cfg(target_os="linux")]
fn init_config() {
    let config = Config {
        wifi_ssid: heapless::String::from_str(config::WIFI_AP).unwrap(),
        wifi_password: heapless::String::from_str(config::WIFI_PASS).unwrap(),
        device_id: config::SENSOR_REMO_DEVICE_ID,
        appliance_id: config::ECHONETLITE_APPLIANCE_ID,
        access_token: heapless::String::from_str(config::ACCESS_TOKEN).unwrap(),
    };
    log::info!("init_config {:?}", config);
    *CONFIG.lock().unwrap() = Some(config);
}

static mut GFX: Option<Gfx> = None;

fn main() -> anyhow::Result<()> {
    // Temporary. Will disappear once ESP-IDF 4.4 is released, but for now it is necessary to call this function once,
    // or else some patches to the runtime implemented by esp-idf-sys might not link properly.
    #[cfg(target_os="espidf")]
    {
        esp_idf_sys::link_patches();
        esp_idf_svc::log::EspLogger::initialize_default();
    }

    println!("Hello, world!");
    #[cfg(target_os="espidf")]
    {
        unsafe { GFX = Some(Gfx::setup().unwrap()); }
        let gfx_shared = unsafe { GFX.as_ref().unwrap().as_shared() };
        let mut gfx = gfx_shared.lock();
        gfx.set_rotation(1);

    }
    #[cfg(target_os="linux")]
    {
        env_logger::init();
        unsafe { GFX = Some(Gfx::setup(320, 240).unwrap()); }
    }

    // Initialize configuration.
    init_config();
    log::info!("CONFIG: {:?}", CONFIG.lock().unwrap().as_ref().unwrap());

    // Initialize WiFi
    #[cfg(target_os="espidf")]
    let mut wifi = {
        let peripherals = Peripherals::take().unwrap();
        let sysloop = esp_idf_svc::eventloop::EspSystemEventLoop::take()?;

        let mut wifi = EspWifi::new(
            peripherals.modem,
            sysloop.clone(),
            None,
        )?;
        {
            log::info!("Configuring Wi-Fi");
            let (ssid, pass) = {
                let guard = CONFIG.lock().unwrap();
                let config = guard.as_ref().unwrap();
                (config.wifi_ssid.clone(), config.wifi_password.clone())
            };
            wifi.set_configuration(&esp_idf_svc::wifi::Configuration::Client(
                esp_idf_svc::wifi::ClientConfiguration {
                    ssid,
                    password: pass,
                    channel: None,
                    ..Default::default()
                },
            ))?;
        }

        let wifi = BlockingWifi::wrap(wifi, sysloop.clone())?;
        wifi
    };

    wifi.start()?;
    wifi.connect()?;
    wifi.wait_netif_up()?;

    #[cfg(target_os="linux")]
    let (wifi, wifi_wait) = {
        (Arc::new(Mutex::new(EspWifi{})), WifiWait {})
    };
    
    #[cfg(target_os="linux")]
    loop { 
        Gfx::handle_sdl_event();
        std::thread::sleep(Duration::from_millis(5)); 
    }

    std::thread::Builder::new().name("capture".into()).stack_size(4096).spawn(|| {
        let socket = UdpSocket::bind(&"0.0.0.0:4000").unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let mut buffer = std::vec::Vec::new();
        buffer.resize(1200, 0u8);

        let gfx_shared = unsafe { GFX.as_ref().unwrap().as_shared() };
        loop {
            if let Ok((size, _)) = socket.recv_from(&mut buffer) {
                if size < 16 {
                    continue;
                }
                let marker = (&buffer[0..2]).read_u16::<LittleEndian>().unwrap();
                let x = (&buffer[2..4]).read_u16::<LittleEndian>().unwrap();
                let y = (&buffer[4..6]).read_u16::<LittleEndian>().unwrap();
                let w = (&buffer[6..8]).read_u16::<LittleEndian>().unwrap();
                let h = (&buffer[8..10]).read_u16::<LittleEndian>().unwrap();
                if marker != 0xaa55 || size < (w as usize) * (h as usize) * 3 {
                    continue;
                }
                let mut gfx = gfx_shared.lock();
                gfx.push_image_rgb888(x as i32, y as i32, w as i32, h as i32, &buffer[16..size]);
            }
            
        }
    }).unwrap();

    {
        let gfx_shared = unsafe { GFX.as_ref().unwrap().as_shared() };
        
        let (w, h) = {
            let mut gfx = gfx_shared.lock();
            gfx.size()
        };
        let _sntp = sntp::EspSntp::new_default()?;
        loop {
            let system_time = std::time::SystemTime::now();
            let date_time: DateTime<Utc> = system_time.into();
            let local_time = date_time.with_timezone(&FixedOffset::east(9*3600));

            let s = local_time.format("%Y-%m-%d %H:%M:%S").to_string();

            let mut gfx = gfx_shared.lock();
            gfx.set_font(lgfx::fonts::AsciiFont24x48).unwrap();
            gfx.draw_string(s.as_str(), 0, 0, ColorRgb888::new(0), ColorRgb888::new(0xffffff), 1.0, 1.0, textdatum_top_left);
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}
